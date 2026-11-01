// Secrets are not in wrangler.jsonc, so `wrangler types` cannot see them.
interface Env {
  /** Signs each recording's upload token. Set with `wrangler secret put SIGNING_KEY`. */
  SIGNING_KEY: string;
  /** The admin key: changes or deletes any recording. Set with `wrangler secret put UPLOAD_KEY`. */
  UPLOAD_KEY: string;
}
