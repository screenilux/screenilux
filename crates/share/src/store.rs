//! The two places a recording can go: the Screenilux storage, through its
//! upload API (web/worker/index.ts), and the user's own bucket, through the
//! S3 API.

use std::time::Duration;

use rusty_s3::{Bucket as S3Bucket, Credentials, S3Action, UrlStyle};
use serde::Deserialize;
use url::Url;

use crate::{Bucket, PLAYER, ShareError};

/// How long a signed bucket request stays valid. Covers a slow part upload.
const SIGNED_FOR: Duration = Duration::from_secs(60 * 60);

pub(crate) trait Store: Send {
    fn put(&self, name: &str, body: &[u8], kind: &str) -> Result<(), ShareError>;
    /// Start a file that goes up in parts; returns the upload's id.
    fn start_parts(&self, name: &str, kind: &str) -> Result<String, ShareError>;
    /// One part, numbered from 1; returns its ETag.
    fn put_part(
        &self,
        name: &str,
        upload: &str,
        number: u16,
        body: &[u8],
    ) -> Result<String, ShareError>;
    fn finish_parts(&self, name: &str, upload: &str, etags: &[String]) -> Result<(), ShareError>;
    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError>;
}

/// The Screenilux storage.
pub(crate) struct Hosted {
    agent: ureq::Agent,
    auth: String,
    /// `<server>/api/recordings/<id>/`
    files: String,
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
#[serde(rename_all = "camelCase")]
struct Started {
    upload_id: String,
}

#[derive(Deserialize)]
struct Part {
    etag: String,
}

impl Hosted {
    /// A new, empty recording on the storage for `total` bytes, and its link.
    pub fn create(
        agent: ureq::Agent,
        server: &str,
        total: u64,
    ) -> Result<(Self, String), ShareError> {
        let server = server.trim_end_matches('/');
        let created: Created = agent
            .post(format!("{server}/api/recordings"))
            .send_json(serde_json::json!({ "total": total }))?
            .body_mut()
            .read_json()?;
        if total > created.limit {
            return Err(ShareError::TooBig);
        }
        let auth = format!("Bearer {}", created.token);
        let files = format!("{server}/api/recordings/{}/", created.id);
        Ok((Self { agent, auth, files }, created.link))
    }
}

impl Store for Hosted {
    fn put(&self, name: &str, body: &[u8], kind: &str) -> Result<(), ShareError> {
        self.agent
            .put(format!("{}{name}", self.files))
            .header("Authorization", &self.auth)
            .header("Content-Type", kind)
            .send(body)?;
        Ok(())
    }

    fn start_parts(&self, name: &str, _kind: &str) -> Result<String, ShareError> {
        let started: Started = self
            .agent
            .post(format!("{}{name}/uploads", self.files))
            .header("Authorization", &self.auth)
            .send_empty()?
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
    ) -> Result<String, ShareError> {
        let part: Part = self
            .agent
            .put(format!("{}{name}/uploads/{upload}/{number}", self.files))
            .header("Authorization", &self.auth)
            .send(body)?
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
        self.agent
            .post(format!("{}{name}/uploads/{upload}", self.files))
            .header("Authorization", &self.auth)
            .send_json(serde_json::json!({ "parts": parts }))?;
        Ok(())
    }

    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError> {
        self.agent
            .delete(format!("{}{name}/uploads/{upload}", self.files))
            .header("Authorization", &self.auth)
            .call()?;
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
    /// A place for a new recording in the bucket, and its link.
    pub fn create(
        agent: ureq::Agent,
        settings: &Bucket,
        id: &str,
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
            "{PLAYER}/b#{}",
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

    fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }
}

impl Store for Own {
    fn put(&self, name: &str, body: &[u8], kind: &str) -> Result<(), ShareError> {
        let key = self.key(name);
        let mut action = self.bucket.put_object(Some(&self.credentials), &key);
        action.headers_mut().insert("content-type", kind);
        self.agent
            .put(action.sign(SIGNED_FOR).as_str())
            .header("Content-Type", kind)
            .send(body)?;
        Ok(())
    }

    fn start_parts(&self, name: &str, kind: &str) -> Result<String, ShareError> {
        let key = self.key(name);
        let mut action = self
            .bucket
            .create_multipart_upload(Some(&self.credentials), &key);
        action.headers_mut().insert("content-type", kind);
        let xml = self
            .agent
            .post(action.sign(SIGNED_FOR).as_str())
            .header("Content-Type", kind)
            .send_empty()?
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
    ) -> Result<String, ShareError> {
        let key = self.key(name);
        let action = self
            .bucket
            .upload_part(Some(&self.credentials), &key, number, upload);
        let response = self
            .agent
            .put(action.sign(SIGNED_FOR).as_str())
            .send(body)?;
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
        self.agent.post(url.as_str()).send(action.body())?;
        Ok(())
    }

    fn abort_parts(&self, name: &str, upload: &str) -> Result<(), ShareError> {
        let key = self.key(name);
        let action = self
            .bucket
            .abort_multipart_upload(Some(&self.credentials), &key, upload);
        self.agent.delete(action.sign(SIGNED_FOR).as_str()).call()?;
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
    fn a_bucket_link_carries_where_the_files_are() {
        let (own, link) =
            Own::create(ureq::Agent::new_with_defaults(), &settings(), "abc").unwrap();
        assert_eq!(own.key("screen1.mp4"), "shared/abc/screen1.mp4");
        assert_eq!(
            link,
            format!("{PLAYER}/b#https%3A%2F%2Fpub-x.r2.dev%2Fshared%2Fabc%2F")
        );
    }

    #[test]
    fn a_bucket_must_be_read_over_https() {
        let mut s = settings();
        s.public_url = "http://pub-x.r2.dev".into();
        assert!(matches!(
            Own::create(ureq::Agent::new_with_defaults(), &s, "abc"),
            Err(ShareError::Bucket(_))
        ));
    }

    #[test]
    fn signed_uploads_go_to_the_bucket_by_path() {
        let (own, _) = Own::create(ureq::Agent::new_with_defaults(), &settings(), "abc").unwrap();
        let key = own.key("share.json");
        let url = own
            .bucket
            .put_object(Some(&own.credentials), &key)
            .sign(SIGNED_FOR);
        assert!(
            url.as_str()
                .starts_with("https://acc.r2.cloudflarestorage.com/videos/shared/abc/share.json?")
        );
        assert!(url.query().unwrap().contains("X-Amz-Signature="));
    }
}
