# Credentials live in a master-password Vault

Credentials are stored in one encrypted Vault file (key derived from the Master Password with Argon2id; contents encrypted with XChaCha20-Poly1305). The Vault is unlocked once when Komando starts. Host definitions refer to Credentials only by reference and never contain secrets. We chose this over Windows Credential Manager so that unlocking is an explicit, user-controlled step and the Vault can be backed up as one file. Windows Hello unlock may be added later as a convenience on top, not as a replacement.

## Consequences

- When the Vault locks (manually or after inactivity), open Sessions stay connected. Only new connections need it unlocked again.
