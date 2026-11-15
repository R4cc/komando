# Credentials live in a master-password Vault

Credentials are stored in one encrypted Vault file (key derived from the Master Password with Argon2id; contents encrypted with XChaCha20-Poly1305). The Vault stays locked until something needs it (connecting with a saved password or key, saving one, importing a key file); Komando then asks for the Master Password in a dialog that says why and that it is not a server's password. Komando itself starts without asking. Host definitions refer to Credentials only by reference and never contain secrets. We chose this over Windows Credential Manager so that unlocking is an explicit, user-controlled step and the Vault can be backed up as one file. Windows Hello unlock may be added later as a convenience on top, not as a replacement.

## Consequences

- When the Vault locks (manually or after inactivity), open Sessions stay connected. Only new connections need it unlocked again.

## Timed unlocks across restarts

On Windows, selecting a timed Auto-lock period with Stay unlocked enabled also remembers
the unlock across application restarts. The derived vault key and its last-use time are
protected by current-user Windows DPAPI in a separate local file. The Master Password is
never saved. This is an explicit exception to the original startup behavior above: an
unexpired remembered unlock restores access without another Master Password prompt.

Time with Komando closed counts toward inactivity. Restoring access does not reset the
last-use time; ordinary keyboard activity extends it as before. Lock now, one-use unlocks,
and Until restart remove the saved access. A changed timeout, clock rollback, corrupted
unlock file, or different vault requires the Master Password again. Failure to restore
never weakens vault encryption or damages the vault file.

This convenience trusts the Windows account for the selected period: software running
as that account can access the DPAPI-protected key. The encrypted Vault remains portable;
the separate convenience file is not part of its backup or recovery format. Until restart
remains the default and retains the original behavior. Linux development builds do not
persist an unlock.
