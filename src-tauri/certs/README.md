# Vendored certificates

## `riotgames.pem`

Riot's root certificate for the game's local HTTPS endpoints. Both the Live Client Data
API (`https://127.0.0.1:2999`) and the League client (LCU) serve leaf certificates
issued by it. Downplayed trusts this root and nothing else for those connections (see
`src/league/tls.rs`).

| | |
|---|---|
| Source | <https://static.developer.riotgames.com/docs/lol/riotgames.pem>, linked from the Game Client API section of <https://developer.riotgames.com/docs/lol> |
| Retrieved | 2026-10-15 (server `Last-Modified: Wed, 26 Feb 2020 04:33:54 GMT`, `ETag: "02e979849537e5958cde25bb4fd8bd35"`) |
| File SHA-256 | `da884275737f024b33c93ae5d28bdb002768a3cb73752ab40254a32218193521` |
| Subject / issuer | `CN=LoL Game Engineering Certificate Authority, OU=LoL Game Engineering, O=Riot Games, L=Santa Monica, ST=California, C=US` (self-signed) |
| Validity | 2013-12-04 → 2043-11-27 |
| Certificate SHA-256 fingerprint | `CA:8C:9D:32:5B:4C:DC:46:4C:6C:94:A5:85:C8:5E:91:EC:23:D4:0B:A5:BF:3A:E2:82:2B:95:1A:4A:50:4E:A3` |

It is an X.509 **v1** certificate with a SHA-1 self-signature. That is fine for a trust
anchor: rustls-webpki accepts v1 roots and never checks a root's own signature. The leaf
it signs, which both the League client and the game serve, is v3,
`sha256WithRSAEncryption`, `CN=rclient`, with SANs `DNS:localhost` and `IP:127.0.0.1`.
It passes strict verification for `127.0.0.1` (captured 2026-10-15, see
`fixtures/tls/rclient-leaf-2026.pem`).

To refresh it, download the file again and compare the fingerprint above. Keep the bytes
exact: `.gitattributes` marks `*.pem` as binary so line endings are never rewritten.
