-- Who shared what, so each device and each network can be held to its
-- allowance (see worker/index.ts). Rows older than a week no longer count and
-- are cleared as new shares arrive.
CREATE TABLE shares (
  id TEXT PRIMARY KEY,
  -- A random id the app makes once per computer.
  device TEXT NOT NULL,
  -- A keyed hash of the address the share came from, not the address itself.
  network TEXT NOT NULL,
  -- Bytes the recording was allowed, as the app declared them.
  bytes INTEGER NOT NULL,
  -- When the share started, in ms since 1970.
  started INTEGER NOT NULL
);
CREATE INDEX shares_device ON shares (device, started);
CREATE INDEX shares_network ON shares (network, started);
