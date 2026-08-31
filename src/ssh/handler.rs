use russh::client;
use russh::keys::known_hosts::{check_known_hosts, learn_known_hosts};
use russh::keys::ssh_key::{self, HashAlg};
use russh::keys::PublicKeyOrCertificate;

use crate::config::HostKeyPolicy;

pub struct ClientHandler {
    pub host: String,
    pub port: u16,
    pub policy: HostKeyPolicy,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key: ssh_key::PublicKey = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => {
                ssh_key::PublicKey::new(cert.public_key().clone(), "")
            }
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256);
        match check_known_hosts(&self.host, self.port, &key) {
            Ok(true) => Ok(true),
            Ok(false) => match self.policy {
                HostKeyPolicy::AcceptNew => {
                    eprintln!(
                        "agentssh: learning new host key for {}:{} ({fingerprint})",
                        self.host, self.port
                    );
                    if let Err(e) = learn_known_hosts(&self.host, self.port, &key) {
                        eprintln!("agentssh: failed to update known_hosts: {e}");
                        return Ok(false);
                    }
                    Ok(true)
                }
                HostKeyPolicy::Strict => {
                    eprintln!(
                        "agentssh: host key for {}:{} is not in known_hosts ({fingerprint}); \
                         refusing under strict policy",
                        self.host, self.port
                    );
                    Ok(false)
                }
            },
            Err(e) => {
                // Includes KeyChanged: a changed key is never accepted, under any policy.
                eprintln!(
                    "agentssh: HOST KEY VERIFICATION FAILED for {}:{} ({fingerprint}): {e}",
                    self.host, self.port
                );
                Ok(false)
            }
        }
    }
}
