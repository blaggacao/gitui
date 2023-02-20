//! Sign commit data.

/// Error type for [`SignBuilder`], used to create [`Sign`]'s
#[derive(thiserror::Error, Debug)]
pub enum SignBuilderError {
	/// The given format is invalid
	#[error("Failed to derive a commit signing method from git configuration 'gpg.format': {0}")]
	InvalidFormat(String),

	/// The GPG signing key could
	#[error("Failed to retrieve 'user.signingkey' from the git configuration: {0}")]
	GPGSigningKey(String),

	/// No signing signature could be built from the configuration data present
	#[error("Failed to build signing signature: {0}")]
	Signature(String),

	/// Failure on unimplemented signing methods
	/// to be removed once all methods have been implemented
	#[error("Select signing method '{0}' has not been implemented")]
	MethodNotImplemented(String),
}

/// Error type for [`Sign`], used to sign data
#[derive(thiserror::Error, Debug)]
pub enum SignError {
	/// Unable to spawn process
	#[error("Failed to spawn signing process: {0}")]
	Spawn(String),

	/// Unable to acquire the child process' standard input to write the commit data for signing
	#[error("Failed to acquire standard input handler")]
	Stdin,

	/// Unable to write commit data to sign to standard input of the child process
	#[error("Failed to write buffer to standard input of signing process: {0}")]
	WriteBuffer(String),

	/// Unable to retrieve the signed data from the child process
	#[error("Failed to get output of signing process call: {0}")]
	Output(String),

	/// Failure of the child process
	#[error("Failed to execute signing process: {0}")]
	Shellout(String),
}

/// Sign commit data using various methods
pub trait Sign {
	/// Sign commit with the respective implementation.
	///
	/// Retrieve an implementation using [`SignBuilder::from_gitconfig`].
	///
	/// The `commit` string slice can be created using the following steps:
	/// - create a buffer using [`git2::Repository::commit_create_buffer`]
	/// - convert the buffer using [`std::str::from_utf8`]
	/// - the resulting string slice can be passed to [`Sign::sign`] now
	///
	/// The returned `String` from this function can then be passed into [`git2::Repository::commit_signed`].
	/// Finally, the repository head needs to be advanced to the resulting commit ID
	/// using [`git2::Reference::set_target`].
	fn sign(&self, commit: &str) -> Result<String, SignError>;

	#[cfg(test)]
	fn program(&self) -> &String;

	#[cfg(test)]
	fn signing_key(&self) -> &String;
}

/// A builder to facilitate the creation of a signing method ([`Sign`]) by examining the git configuration.
pub struct SignBuilder;

impl SignBuilder {
	/// Get a [`Sign`] from the given repository configuration to sign commit data
	///
	///
	/// ```no_run
	/// use asyncgit::sync::sign::SignBuilder;
	/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
	///
	/// /// Repo in a temporary directory for demonstration
	/// let dir = std::env::temp_dir();
	/// let repo = git2::Repository::init(dir)?;
	///
	/// /// Get the config from the repository
	/// let config = repo.config()?;
	///
	/// /// Retrieve a `Sign` implementation
	/// let sign = SignBuilder::from_gitconfig(&repo, &config)?;
	/// # Ok(())
	/// # }
	/// ```
	pub fn from_gitconfig(
		repo: &git2::Repository,
		config: &git2::Config,
	) -> Result<impl Sign, SignBuilderError> {
		let signing_methods = config
			.get_string("gitui.signing_methods")
			.unwrap_or_else(|_| "shellouts".to_string());

		match signing_methods.as_str() {
			"shellouts" => {
				let format = config
					.get_string("gpg.format")
					.unwrap_or_else(|_| "openpgp".to_string());

				// Variants are described in the git config documentation
				// https://git-scm.com/docs/git-config#Documentation/git-config.txt-gpgformat
				match format.as_str() {
					"openpgp" => {
						// Try to retrieve the gpg program from the git configuration,
						// moving from the least to the most specific config key,
						// defaulting to "gpg" if nothing is explicitly defined (per git's implementation)
						// https://git-scm.com/docs/git-config#Documentation/git-config.txt-gpgprogram
						// https://git-scm.com/docs/git-config#Documentation/git-config.txt-gpgprogram
						let program = config
							.get_string("gpg.openpgp.program")
							.or_else(|_| {
								config.get_string("gpg.program")
							})
							.unwrap_or_else(|_| "gpg".to_string());

						// Optional signing key.
						// If 'user.signingKey' is not set, we'll use 'user.name' and 'user.email'
						// to build a default signature in the format 'name <email>'.
						// https://git-scm.com/docs/git-config#Documentation/git-config.txt-usersigningKey
						let signing_key = config
							.get_string("user.signingKey")
							.or_else(
								|_| -> Result<
									String,
									SignBuilderError,
								> {
									Ok(crate::sync::commit::signature_allow_undefined_name(repo)
										.map_err(|err| {
											SignBuilderError::Signature(
												err.to_string(),
											)
										})?
										.to_string())
								},
							)
							.map_err(|err| {
								SignBuilderError::GPGSigningKey(
									err.to_string(),
								)
							})?;

						Ok(GPGSign {
							program,
							signing_key,
						})
					}
					"x509" => {
						Err(SignBuilderError::MethodNotImplemented(
							String::from("x509"),
						))
					}
					"ssh" => {
						Err(SignBuilderError::MethodNotImplemented(
							String::from("ssh"),
						))
					}
					_ => Err(SignBuilderError::InvalidFormat(format)),
				}
			}
			"rust" => Err(SignBuilderError::MethodNotImplemented(
				String::from("<rust native>"),
			)),
			_ => {
				Err(SignBuilderError::InvalidFormat(signing_methods))
			}
		}
	}
}

/// Sign commit data using `OpenPGP`
pub struct GPGSign {
	program: String,
	signing_key: String,
}

impl GPGSign {
	/// Create new [`GPGSign`] using given program and signing key.
	pub fn new(program: &str, signing_key: &str) -> Self {
		Self {
			program: program.to_string(),
			signing_key: signing_key.to_string(),
		}
	}
}

impl Sign for GPGSign {
	fn sign(&self, commit: &str) -> Result<String, SignError> {
		use std::io::Write;
		use std::process::{Command, Stdio};

		let mut cmd = Command::new(&self.program);
		cmd.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.arg("--status-fd=2")
			.arg("-bsau")
			.arg(&self.signing_key);

		log::trace!("signing command: {cmd:?}");

		let mut child = cmd
			.spawn()
			.map_err(|e| SignError::Spawn(e.to_string()))?;

		let mut stdin = child.stdin.take().ok_or(SignError::Stdin)?;

		write!(stdin, "{commit}")
			.map_err(|e| SignError::WriteBuffer(e.to_string()))?;
		drop(stdin); // close stdin to not block indefinitely

		let output = child
			.wait_with_output()
			.map_err(|e| SignError::Output(e.to_string()))?;

		if !output.status.success() {
			return Err(SignError::Shellout(format!(
				"failed to sign data, program '{}' exited non-zero: {}",
				&self.program,
				std::str::from_utf8(&output.stderr)
					.unwrap_or("[error could not be read from stderr]")
			)));
		}

		let stderr = std::str::from_utf8(&output.stderr)
			.map_err(|e| SignError::Shellout(e.to_string()))?;

		if !stderr.contains("\n[GNUPG:] SIG_CREATED ") {
			return Err(SignError::Shellout(
				format!("failed to sign data, program '{}' failed, SIG_CREATED not seen in stderr", &self.program),
			));
		}

		let signed_commit = std::str::from_utf8(&output.stdout)
			.map_err(|e| SignError::Shellout(e.to_string()))?;

		Ok(signed_commit.to_string())
	}

	#[cfg(test)]
	fn program(&self) -> &String {
		&self.program
	}

	#[cfg(test)]
	fn signing_key(&self) -> &String {
		&self.signing_key
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::error::Result;
	use crate::sync::tests::repo_init_empty;

	#[test]
	fn test_invalid_signing_format() -> Result<()> {
		let (_temp_dir, repo) = repo_init_empty()?;

		{
			let mut config = repo.config()?;
			config.set_str("gpg.format", "INVALID_SIGNING_FORMAT")?;
		}

		let sign =
			SignBuilder::from_gitconfig(&repo, &repo.config()?);

		assert!(sign.is_err());

		Ok(())
	}

	#[test]
	fn test_program_and_signing_key_defaults() -> Result<()> {
		let (_tmp_dir, repo) = repo_init_empty()?;
		let sign =
			SignBuilder::from_gitconfig(&repo, &repo.config()?)?;

		assert_eq!("gpg", sign.program());
		assert_eq!("name <email>", sign.signing_key());

		Ok(())
	}

	#[test]
	fn test_gpg_program_configs() -> Result<()> {
		let (_tmp_dir, repo) = repo_init_empty()?;

		{
			let mut config = repo.config()?;
			config.set_str("gpg.program", "GPG_PROGRAM_TEST")?;
		}

		let sign =
			SignBuilder::from_gitconfig(&repo, &repo.config()?)?;

		// we get gpg.program, because gpg.openpgp.program is not set
		assert_eq!("GPG_PROGRAM_TEST", sign.program());

		{
			let mut config = repo.config()?;
			config.set_str(
				"gpg.openpgp.program",
				"GPG_OPENPGP_PROGRAM_TEST",
			)?;
		}

		let sign =
			SignBuilder::from_gitconfig(&repo, &repo.config()?)?;

		// since gpg.openpgp.program is now set as well, it is more specific than
		// gpg.program and therefore takes precedence
		assert_eq!("GPG_OPENPGP_PROGRAM_TEST", sign.program());

		Ok(())
	}

	#[test]
	fn test_user_signingkey() -> Result<()> {
		let (_tmp_dir, repo) = repo_init_empty()?;

		{
			let mut config = repo.config()?;
			config.set_str("user.signingKey", "FFAA")?;
		}

		let sign =
			SignBuilder::from_gitconfig(&repo, &repo.config()?)?;

		assert_eq!("FFAA", sign.signing_key());

		Ok(())
	}
}
