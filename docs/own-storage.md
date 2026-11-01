# Sharing to your own storage

Screenilux can upload recordings to storage you own, instead of to
Screenilux. Your recordings stay there until you delete them, and the keys
you enter never leave your computer.

Any storage that speaks the S3 protocol works: Cloudflare R2, Amazon S3,
Backblaze B2, Wasabi and others. The steps below use Cloudflare R2, which has
a free allowance and charges nothing for people watching your recordings.

## 1. Make a bucket

In the Cloudflare dashboard, open **R2 Object Storage** and choose
**Create bucket**. Call it something like `recordings`.

## 2. Let anyone with a link read it

People watching your links read the videos straight from the bucket, so it
needs a public address.

1. Open the bucket, then **Settings**.
2. Under **Public access**, either turn on the **R2.dev subdomain** (fine to
   start with, but Cloudflare limits how much it serves), or connect a
   **custom domain** you own.
3. Copy the public address, for example `https://pub-1234abcd.r2.dev`.

This doesn't make your recordings public. Screenilux encrypts every file
before uploading it, and the key is only in the link, so anyone reading the
bucket gets scrambled bytes. Each recording is also kept under a long, random
name.

## 3. Let the Screenilux page read it

Still in the bucket's **Settings**, under **CORS policy**, choose **Add CORS
policy** and paste:

```json
[
  {
    "AllowedOrigins": ["https://screenilux.hassen-ben-mbarek.workers.dev"],
    "AllowedMethods": ["GET", "HEAD"],
    "AllowedHeaders": ["Range"],
    "ExposeHeaders": ["Content-Length", "Content-Range", "Accept-Ranges"],
    "MaxAgeSeconds": 3600
  }
]
```

## 4. Make keys for Screenilux

1. Back on the **R2 Object Storage** page, choose **Manage API tokens**, then
   **Create API token**.
2. Give it **Object Read & Write** permission, for this bucket only.
3. Create it, then keep the page open: the secret is shown only once.

## 5. Fill it in

In Screenilux, open **More options**, set **Share a link when you stop** to
**My bucket**, and fill in:

| Field | What to enter |
|---|---|
| S3 address | The **S3 API** address from the token page, without the bucket name: `https://<account id>.r2.cloudflarestorage.com` |
| Bucket | The bucket's name, e.g. `recordings` |
| Region | `auto` |
| Access key ID | From the token page |
| Secret access key | From the token page |
| Public address | The address from step 2 |
| Folder | Optional. Recordings go in this folder of the bucket |

Screenilux saves these as you type. The next recording you stop is uploaded
and its link is copied.

## Keeping it tidy

Recordings stay in your bucket until you delete them. To have them deleted
automatically, open the bucket's **Settings**, then **Object lifecycle
rules**, and add a rule that deletes objects after the number of days you
want.

## Other services

For Amazon S3, Backblaze B2 or Wasabi, the steps are the same in spirit:

- a bucket whose files anyone can read at a public HTTPS address,
- a CORS rule like the one above,
- an access key that can write to the bucket,
- the service's S3 address and region in Screenilux.
