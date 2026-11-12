//! The two places a recording can go: the Screenilux storage, through its
//! upload API (web/worker/index.ts), and the user's own bucket, through the
//! S3 API. Either way the files are already encrypted, so they go up as plain
//! bytes.

use std::time::Duration;

use rusty_s3::{Bucket as S3Bucket, Credentials, S3Action, UrlStyle};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Bucket, PLAYER, ShareError};

/// How long a signed bucket request stays valid. Covers a slow part upload.
const SIGNED_FOR: Duration = Duration::from_secs(60 * 60);

/// Encrypted files say nothing about what is inside.
const KIND: &str = "application/octet-stream";

/// Bytes handed out so far are reported here, so progress moves during a
/// long request instead of jumping when it ends.
pub(crate) type Sent<'a> = &'a mut dyn FnMut(usize);

/// The agent every request goes through. A refusal is read like any other
/// answer, so the reason the storage gives can be kept (see [`answer`]).
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    )
}

/// The response, if the storage took the request; otherwise what it said
/// about why not.
pub(crate) fn answer(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, ShareError> {
    let mut response = response?;
    if response.status().is_success() {
        return Ok(response);
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_to_string()
        .unwrap_or_default();
    Err(ShareError::refused(response.status().as_u16(), &text))
}

/// Send `body` as the request's body, reporting each piece as it goes out.
fn send_counted(
    request: ureq::RequestBuilder<ureq::typestate::WithBody>,
    body: &[u8],
    sent: Sent<'_>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let mut reader = Counted { rest: body, sent };
    request
        .header("Content-Length", body.len().to_string())
        .send(ureq::SendBody::from_reader(&mut reader))
}

struct Counted<'a, 'b> {
    rest: &'a [u8],
    sent: Sent<'b>,
}

impl std::io::Read for Counted<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.rest.read(buf)?;
        (self.sent)(n);
        Ok(n)
    }
}

/// What it takes to add to a share after it was made: kept with its link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rights {
    /// The recording's address on the Screenilux storage and its upload
    /// token, which works for that recording only.
    Screenilux { recording: String, token: String },
    /// Where its files are in the user's own bucket.
    Bucket { prefix: String },
}

pub(crate) trait Store: Send {
    /// What it takes to add to this share later.
    fn rights(&self) -> Rights;
    fn put(&self, name: &str, body: &[u8], sent: Sent<'_>) -> Result<(), ShareError>;
    /// Start a file that goes up in parts; returns the upload's id.
    fn start_parts(&self, name: &str) -> Result<String, ShareError>;
    /// One part, numbered from 1; returns its ETag.
    fn put_part(
        &self,
        name: &str,
        upload: &str,
        number: u16,
        body: &[u8],
        sent: Sent<'_>,
    ) -> Result<String, ShareError>;
    fn finish_parts(&self, name: &str, upload: &str, etags: &[String]) -> Result<(), ShareError>;
    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError>;
    /// Say that every file is up.
    fn complete(&self) -> Result<(), ShareError>;
    /// Remove what an upload that failed had sent.
    fn discard(&self, names: &[String]) -> Result<(), ShareError>;
}

/// The Screenilux storage.
pub(crate) struct Hosted {
    agent: ureq::Agent,
    auth: String,
    token: String,
    /// `<server>/api/recordings/<id>`
    recording: String,
}

#[derive(Deserialize)]
struct Created {
    id: String,
    link: String,
    /// Lets this upload write to this recording, and nothing else.
    token: String,
    /// The most the recording may hold, in bytes.
    limit: u64,
}

#[derive(Deserialize)]
struct Problem {
    error: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Started {
    upload_id: String,
}

#[derive(Deserialize)]
struct Part {
    etag: String,
}

impl Hosted {
    /// A new, empty recording on the storage for `total` bytes, counted
    /// against `device`'s allowance, and its link.
    pub fn create(
        agent: ureq::Agent,
        server: &str,
        device: &str,
        total: u64,
    ) -> Result<(Self, String), ShareError> {
        let server = server.trim_end_matches('/');
        let mut response = agent
            .post(format!("{server}/api/recordings"))
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(serde_json::json!({ "total": total, "device": device }))?;
        let status = response.status().as_u16();
        if status == 429 {
            return Err(ShareError::TooMany);
        }
        if !response.status().is_success() {
            // The storage explains a refusal, such as no room left, in words
            // to show as they are.
            let text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(match serde_json::from_str::<Problem>(&text) {
                Ok(p) if status == 413 => ShareError::Declined(p.error),
                _ => ShareError::refused(status, &text),
            });
        }
        let created: Created = response.body_mut().read_json()?;
        if total > created.limit {
            return Err(ShareError::Declined(
                "the storage allowed less than this recording needs".into(),
            ));
        }
        let recording = format!("{server}/api/recordings/{}", created.id);
        Ok((
            Self::resume(agent, &recording, &created.token),
            created.link,
        ))
    }

    /// A recording already on the storage, to add to it.
    pub fn resume(agent: ureq::Agent, recording: &str, token: &str) -> Self {
        Self {
            agent,
            auth: format!("Bearer {token}"),
            token: token.to_string(),
            recording: recording.to_string(),
        }
    }
}

impl Store for Hosted {
    fn rights(&self) -> Rights {
        Rights::Screenilux {
            recording: self.recording.clone(),
            token: self.token.clone(),
        }
    }

    fn put(&self, name: &str, body: &[u8], sent: Sent<'_>) -> Result<(), ShareError> {
        let request = self
            .agent
            .put(format!("{}/{name}", self.recording))
            .header("Authorization", &self.auth)
            .header("Content-Type", KIND);
        answer(send_counted(request, body, sent))?;
        Ok(())
    }

    fn start_parts(&self, name: &str) -> Result<String, ShareError> {
        let started: Started = answer(
            self.agent
                .post(format!("{}/{name}/uploads", self.recording))
                .header("Authorization", &self.auth)
                .send_empty(),
        )?
        .body_mut()
        .read_json()?;
        Ok(started.upload_id)
    }

    fn put_part(
        &self,
        name: &str,
        upload: &str,
        number: u16,
        body: &[u8],
        sent: Sent<'_>,
    ) -> Result<String, ShareError> {
        let request = self
            .agent
            .put(format!(
                "{}/{name}/uploads/{upload}/{number}",
                self.recording
            ))
            .header("Authorization", &self.auth);
        let part: Part = answer(send_counted(request, body, sent))?
            .body_mut()
            .read_json()?;
        Ok(part.etag)
    }

    fn finish_parts(&self, name: &str, upload: &str, etags: &[String]) -> Result<(), ShareError> {
        let parts: Vec<_> = etags
            .iter()
            .enumerate()
            .map(|(i, etag)| serde_json::json!({ "partNumber": i + 1, "etag": etag }))
            .collect();
        answer(
            self.agent
                .post(format!("{}/{name}/uploads/{upload}", self.recording))
                .header("Authorization", &self.auth)
                .send_json(serde_json::json!({ "parts": parts })),
        )?;
        Ok(())
    }

    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError> {
        answer(
            self.agent
                .delete(format!("{}/{name}/uploads/{upload}", self.recording))
                .header("Authorization", &self.auth)
                .call(),
        )?;
        Ok(())
    }

    fn complete(&self) -> Result<(), ShareError> {
        answer(
            self.agent
                .post(format!("{}/complete", self.recording))
                .header("Authorization", &self.auth)
                .send_empty(),
        )?;
        Ok(())
    }

    fn discard(&self, _names: &[String]) -> Result<(), ShareError> {
        answer(
            self.agent
                .delete(&self.recording)
                .header("Authorization", &self.auth)
                .call(),
        )?;
        Ok(())
    }
}

/// The user's own bucket.
pub(crate) struct Own {
    agent: ureq::Agent,
    bucket: S3Bucket,
    credentials: Credentials,
    /// `<folder>/<id>/`, the start of every object's key.
    prefix: String,
}

impl Own {
    /// A place for a new recording in the bucket, and its link, which
    /// carries `key`.
    pub fn create(
        agent: ureq::Agent,
        settings: &Bucket,
        id: &str,
        key: &str,
    ) -> Result<(Self, String), ShareError> {
        let bad = |what: &str| ShareError::Bucket(what.to_string());
        let endpoint = Url::parse(settings.endpoint.trim())
            .map_err(|_| bad("the S3 address is not a web address"))?;
        let region = match settings.region.trim() {
            "" => "auto".to_string(),
            r => r.to_string(),
        };
        let bucket = S3Bucket::new(
            endpoint,
            UrlStyle::Path,
            settings.bucket.trim().to_string(),
            region,
        )
        .map_err(|e| ShareError::Bucket(e.to_string()))?;
        let credentials = Credentials::new(settings.access_key.trim(), settings.secret_key.trim());

        let folder = settings.folder.trim().trim_matches('/');
        let prefix = if folder.is_empty() {
            format!("{id}/")
        } else {
            format!("{folder}/{id}/")
        };
        let public = Url::parse(settings.public_url.trim())
            .map_err(|_| bad("the public address is not a web address"))?;
        // The player is served over HTTPS, so browsers refuse anything else,
        // except from this computer while testing.
        let local = matches!(public.host_str(), Some("localhost" | "127.0.0.1"));
        if public.scheme() != "https" && !local {
            return Err(bad("the public address must start with https://"));
        }
        let files = format!("{}/{prefix}", public.as_str().trim_end_matches('/'));
        let link = format!(
            "{PLAYER}/b#k={key}&u={}",
            url::form_urlencoded::byte_serialize(files.as_bytes()).collect::<String>()
        );
        Ok((
            Self {
                agent,
                bucket,
                credentials,
                prefix,
            },
            link,
        ))
    }

    /// A share already in the bucket at `prefix`, to add to it.
    pub fn resume(agent: ureq::Agent, settings: &Bucket, prefix: &str) -> Result<Self, ShareError> {
        let (mut own, _) = Self::create(agent, settings, "resume", "")?;
        own.prefix = prefix.to_string();
        Ok(own)
    }

    fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }
}

impl Store for Own {
    fn rights(&self) -> Rights {
        Rights::Bucket {
            prefix: self.prefix.clone(),
        }
    }

    fn put(&self, name: &str, body: &[u8], sent: Sent<'_>) -> Result<(), ShareError> {
        let key = self.key(name);
        let mut action = self.bucket.put_object(Some(&self.credentials), &key);
        action.headers_mut().insert("content-type", KIND);
        let request = self
            .agent
            .put(action.sign(SIGNED_FOR).as_str())
            .header("Content-Type", KIND);
        answer(send_counted(request, body, sent))?;
        Ok(())
    }

    fn start_parts(&self, name: &str) -> Result<String, ShareError> {
        let key = self.key(name);
        let mut action = self
            .bucket
            .create_multipart_upload(Some(&self.credentials), &key);
        action.headers_mut().insert("content-type", KIND);
        let xml = answer(
            self.agent
                .post(action.sign(SIGNED_FOR).as_str())
                .header("Content-Type", KIND)
                .send_empty(),
        )?
        .body_mut()
        .read_to_string()?;
        let created =
            rusty_s3::actions::CreateMultipartUpload::parse_response(&xml).map_err(|e| {
                ShareError::Bucket(format!("an unexpected answer from the bucket: {e}"))
            })?;
        Ok(created.upload_id().to_string())
    }

    fn put_part(
        &self,
        name: &str,
        upload: &str,
        number: u16,
        body: &[u8],
        sent: Sent<'_>,
    ) -> Result<String, ShareError> {
        let key = self.key(name);
        let action = self
            .bucket
            .upload_part(Some(&self.credentials), &key, number, upload);
        let response = answer(send_counted(
            self.agent.put(action.sign(SIGNED_FOR).as_str()),
            body,
            sent,
        ))?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ShareError::Bucket(
                    "the bucket did not confirm a part; check its CORS and S3 settings".into(),
                )
            })?;
        Ok(etag.to_string())
    }

    fn finish_parts(&self, name: &str, upload: &str, etags: &[String]) -> Result<(), ShareError> {
        let key = self.key(name);
        let action = self.bucket.complete_multipart_upload(
            Some(&self.credentials),
            &key,
            upload,
            etags.iter().map(String::as_str),
        );
        let url = action.sign(SIGNED_FOR);
        answer(self.agent.post(url.as_str()).send(action.body()))?;
        Ok(())
    }

    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError> {
        let key = self.key(name);
        let action = self
            .bucket
            .abort_multipart_upload(Some(&self.credentials), &key, upload);
        answer(self.agent.delete(action.sign(SIGNED_FOR).as_str()).call())?;
        Ok(())
    }

    /// Nothing to say: the manifest, uploaded last, is what tells the player
    /// the recording is ready.
    fn complete(&self) -> Result<(), ShareError> {
        Ok(())
    }

    fn discard(&self, names: &[String]) -> Result<(), ShareError> {
        for name in names {
            let key = self.key(name);
            let action = self.bucket.delete_object(Some(&self.credentials), &key);
            answer(self.agent.delete(action.sign(SIGNED_FOR).as_str()).call())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Bucket {
        Bucket {
            endpoint: "https://acc.r2.cloudflarestorage.com".into(),
            region: String::new(),
            bucket: "videos".into(),
            access_key: "AK".into(),
            secret_key: "SK".into(),
            public_url: "https://pub-x.r2.dev/".into(),
            folder: "/shared/".into(),
        }
    }

    #[test]
    fn a_bucket_link_carries_the_key_and_where_the_files_are() {
        let (own, link) =
            Own::create(ureq::Agent::new_with_defaults(), &settings(), "abc", "KEY").unwrap();
        assert_eq!(own.key("screen1.mp4"), "shared/abc/screen1.mp4");
        assert_eq!(
            link,
            format!("{PLAYER}/b#k=KEY&u=https%3A%2F%2Fpub-x.r2.dev%2Fshared%2Fabc%2F")
        );
    }

    #[test]
    fn a_bucket_must_be_read_over_https() {
        let mut s = settings();
        s.public_url = "http://pub-x.r2.dev".into();
        assert!(matches!(
            Own::create(ureq::Agent::new_with_defaults(), &s, "abc", "KEY"),
            Err(ShareError::Bucket(_))
        ));
    }

    #[test]
    fn signed_uploads_go_to_the_bucket_by_path() {
        let (own, _) =
            Own::create(ureq::Agent::new_with_defaults(), &settings(), "abc", "KEY").unwrap();
        let key = own.key("recording.json");
        let url = own
            .bucket
            .put_object(Some(&own.credentials), &key)
            .sign(SIGNED_FOR);
        assert!(
            url.as_str().starts_with(
                "https://acc.r2.cloudflarestorage.com/videos/shared/abc/recording.json?"
            )
        );
        assert!(url.query().unwrap().contains("X-Amz-Signature="));
    }
}
