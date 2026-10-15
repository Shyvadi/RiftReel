# TLS fixtures

`rclient-leaf-2026.pem` is the public server certificate Riot's local endpoints
presented on 2026-10-15. It was captured with a bare TLS handshake
(`openssl s_client -showcerts`); no credentials were used, and there is no private key.

The League client (LCU, random port) and the running game (Live Client Data API, port
2999) served the **same** certificate (identical SHA-256 fingerprint), over TLS 1.3:

- Subject `CN=rclient`; issuer: Riot's "LoL Game Engineering Certificate Authority"
- SAN `DNS:localhost`, `IP:127.0.0.1`; EKU serverAuth; `CA:FALSE`; RSA-2048, sha256WithRSAEncryption
- Valid 2026-01-04 → 2125-12-11
- SHA-256 fingerprint `23:17:88:E9:B3:24:45:B6:3D:92:C8:79:31:61:02:03:E3:22:D8:EA:DC:65:D2:17:73:70:7D:78:A6:C9:3B:2C`

`league::tls` tests check that it chains to the vendored `riotgames.pem` and passes
rustls' strict verification for `127.0.0.1`.
