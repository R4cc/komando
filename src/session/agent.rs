//! The SSH Agent: signing in with keys it holds, and Agent Forwarding (see
//! docs/adr/0017-signing-in-with-the-ssh-agent.md).
//!
//! The agent keeps the private keys; Komando only asks it for the public halves and for
//! signatures. Keys that never leave a device (a YubiKey, Secretive's Secure Enclave keys) or
//! another program's vault (1Password, Bitwarden, KeePassXC) work the same as `ssh-agent`'s.

use anyhow::{Result, anyhow};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::{AgentIdentity, AgentSessionBinding};
use russh::keys::HashAlg;

/// A connection to the agent that signs.
pub type Client = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// The pipe the Windows OpenSSH agent service (and 1Password on Windows) listens on.
pub const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// How long the agent may take to list its keys. Some ask the user first (1Password's
/// approval, KeePassXC with a locked database), so this is generous.
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Where Komando reaches the SSH Agent, as Settings → Security's "SSH agent" names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Agent {
    /// A Unix socket at this path, or on Windows a named pipe.
    At(String),
    /// Windows without a setting: the OpenSSH agent's pipe, then Pageant.
    Windows,
    /// Pageant (Windows only).
    Pageant,
}

impl Agent {
    /// The agent the setting names. Empty finds the usual one: `SSH_AUTH_SOCK`, and on Windows
    /// without it, the OpenSSH agent's pipe and then Pageant. `none` switches the agent off, as
    /// `IdentityAgent none` does for `ssh`; `pageant` picks Pageant; anything else is a path,
    /// where `~` is the home folder.
    pub fn from_setting(setting: &str) -> Option<Agent> {
        let setting = setting.trim();
        if setting.eq_ignore_ascii_case("none") {
            return None;
        }
        if setting.eq_ignore_ascii_case("pageant") {
            return Some(Agent::Pageant);
        }
        if !setting.is_empty() {
            return Some(Agent::At(expand_home(setting, dirs::home_dir().as_deref())));
        }
        match std::env::var("SSH_AUTH_SOCK").ok().filter(|sock| !sock.trim().is_empty()) {
            Some(sock) => Some(Agent::At(sock)),
            None if cfg!(windows) => Some(Agent::Windows),
            None => None,
        }
    }

    /// How the connection log names it.
    pub fn label(&self) -> String {
        match self {
            Agent::At(path) => path.clone(),
            Agent::Windows => "the OpenSSH agent or Pageant".into(),
            Agent::Pageant => "Pageant".into(),
        }
    }

    /// The agent's keys, bound to the connection if `binding` is given, and the connection to
    /// sign with. `Ok(None)`: no agent is running where Komando looks by default on Windows,
    /// which is not worth a word. An agent named by a path that can't be reached is an error.
    pub async fn open(&self, binding: Option<&AgentSessionBinding>) -> Result<Option<Opened>> {
        if *self != Agent::Windows {
            return self.open_one(binding).await.map(Some);
        }
        // The OpenSSH agent service often runs empty beside Pageant: take the first that has
        // keys.
        match Agent::At(OPENSSH_PIPE.into()).open_one(binding).await {
            Ok(opened) if !opened.keys.is_empty() => return Ok(Some(opened)),
            Ok(_) => {}
            Err(err) => log::debug!("OpenSSH agent: {err:#}"),
        }
        match Agent::Pageant.open_one(binding).await {
            Ok(opened) => Ok(Some(opened)),
            Err(err) => {
                log::debug!("Pageant: {err:#}");
                Ok(None)
            }
        }
    }

    /// [`Self::open`] for one agent (not [`Agent::Windows`]).
    async fn open_one(&self, binding: Option<&AgentSessionBinding>) -> Result<Opened> {
        let mut client = self.client().await?;
        if let Some(binding) = binding
            && let Err(err) = client.bind_session(binding, false).await
        {
            // Agents without OpenSSH's extension still sign; only keys limited to certain
            // servers need it.
            log::debug!("{}: no session binding: {err}", self.label());
        }
        let keys = tokio::time::timeout(LIST_TIMEOUT, client.request_identities())
            .await
            .map_err(|_| anyhow!("{} did not list its keys in time", self.label()))?
            .map_err(|err| anyhow!("{} did not list its keys: {err}", self.label()))?;
        Ok(Opened { client, keys, agent: self.clone() })
    }

    /// The agent's keys, for picking one in the host form, once each: a key and its
    /// certificate are the same key there. Empty when no agent is running.
    pub async fn keys(&self) -> Result<Vec<Key>> {
        let mut keys: Vec<Key> = vec![];
        for key in self.open(None).await?.map(|opened| opened.keys).unwrap_or_default().iter().map(Key::of) {
            if !keys.iter().any(|k| k.fingerprint == key.fingerprint) {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    /// A connection to the agent. [`Agent::Windows`] takes the OpenSSH agent's pipe if it is
    /// there, else Pageant.
    async fn client(&self) -> Result<Client> {
        match self {
            #[cfg(windows)]
            Agent::Windows => match Agent::At(OPENSSH_PIPE.into()).connect().await {
                Ok(client) => Ok(client),
                Err(_) => Agent::Pageant.connect().await,
            },
            _ => self.connect().await,
        }
    }

    async fn connect(&self) -> Result<Client> {
        match self {
            #[cfg(unix)]
            Agent::At(path) => Ok(AgentClient::connect_uds(path)
                .await
                .map_err(|err| anyhow!("Couldn't reach the SSH agent at {path}: {err}"))?
                .dynamic()),
            #[cfg(windows)]
            Agent::At(path) => Ok(AgentClient::connect_named_pipe(path)
                .await
                .map_err(|err| anyhow!("Couldn't reach the SSH agent at {path}: {err}"))?
                .dynamic()),
            #[cfg(windows)]
            Agent::Pageant => {
                Ok(AgentClient::connect_pageant().await.map_err(|err| anyhow!("Couldn't reach Pageant: {err}"))?.dynamic())
            }
            // Not reached: only `client` calls this, and it picks the agent for `Agent::Windows`
            // itself. Kept so the match stays whole.
            #[cfg(windows)]
            Agent::Windows => Err(anyhow!("No agent chosen.")),
            #[cfg(not(windows))]
            Agent::Pageant | Agent::Windows => Err(anyhow!("Pageant runs on Windows only.")),
        }
    }

    /// Carry a forwarded agent channel from the server to this agent until either end closes.
    /// OpenSSH's agent is told which server the requests come through, as OpenSSH's client does, so
    /// keys limited to certain servers keep their limits.
    pub(crate) async fn relay(&self, channel: russh::Channel<russh::client::Msg>, binding: Option<AgentSessionBinding>) {
        // On Windows, the agent that has keys, as for signing in.
        let agent = match self {
            Agent::Windows => match self.open(None).await {
                Ok(Some(opened)) => opened.agent,
                _ => return,
            },
            _ => self.clone(),
        };
        let mut client = match agent.client().await {
            Ok(client) => client,
            Err(err) => {
                log::warn!("agent forwarding: {err:#}");
                return;
            }
        };
        if let Some(binding) = &binding
            && let Err(err) = client.bind_session(binding, true).await
        {
            log::debug!("{}: no session binding for forwarding: {err}", agent.label());
        }
        let mut agent = client.into_inner();
        let mut channel = channel.into_stream();
        if let Err(err) = tokio::io::copy_bidirectional(&mut channel, &mut agent).await {
            log::debug!("agent forwarding ended: {err}");
        }
    }
}

/// An agent that answered, with the keys it offers.
pub struct Opened {
    pub client: Client,
    pub keys: Vec<AgentIdentity>,
    /// The agent that answered: on Windows, which of the two.
    pub agent: Agent,
}

/// One of the agent's keys, as the host form lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// `SHA256:…`, which a Host saves to sign in with only this key.
    pub fingerprint: String,
    /// "ssh-ed25519", "ecdsa-sha2-nistp256", …; a certificate's key type.
    pub algorithm: String,
    /// What the agent calls it: often a file name or an e-mail address, sometimes empty.
    pub comment: String,
    pub certificate: bool,
}

impl Key {
    pub fn of(identity: &AgentIdentity) -> Self {
        let (comment, certificate) = match identity {
            AgentIdentity::PublicKey { comment, .. } => (comment.clone(), false),
            AgentIdentity::Certificate { comment, .. } => (comment.clone(), true),
        };
        let key = identity.public_key();
        Key { fingerprint: fingerprint(identity), algorithm: key.algorithm().to_string(), comment, certificate }
    }

    /// Its comment, or its fingerprint when it has none.
    pub fn label(&self) -> &str {
        if self.comment.trim().is_empty() { &self.fingerprint } else { &self.comment }
    }
}

/// The SHA-256 fingerprint of an agent key: what a Host saves to sign in with only that key.
/// A certificate counts as the key it certifies.
pub fn fingerprint(identity: &AgentIdentity) -> String {
    identity.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// `path` with a leading `~` as the home folder.
fn expand_home(path: &str, home: Option<&std::path::Path>) -> String {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            format!("{}{rest}", home.display())
        }
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_names_the_agent() {
        assert_eq!(Agent::from_setting("none"), None);
        assert_eq!(Agent::from_setting(" NONE "), None);
        assert_eq!(Agent::from_setting("Pageant"), Some(Agent::Pageant));
        assert_eq!(Agent::from_setting("/run/agent.sock"), Some(Agent::At("/run/agent.sock".into())));
    }

    #[test]
    fn a_path_may_start_at_home() {
        let home = std::path::Path::new("/home/ada");
        assert_eq!(expand_home("~/.1password/agent.sock", Some(home)), "/home/ada/.1password/agent.sock");
        assert_eq!(expand_home("~", Some(home)), "/home/ada");
        assert_eq!(expand_home("~bob/agent.sock", Some(home)), "~bob/agent.sock");
        assert_eq!(expand_home("/tmp/agent.sock", Some(home)), "/tmp/agent.sock");
    }
}
