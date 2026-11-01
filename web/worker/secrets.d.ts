// Secrets are not in wrangler.jsonc, so `wrangler types` cannot see them.
interface Env {
  /** What the desktop app sends to upload. Set with `wrangler secret put UPLOAD_KEY`. */
  UPLOAD_KEY: string;
}
