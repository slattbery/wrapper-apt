use std::{
	borrow::Cow,
	ffi::OsStr,
	os::unix::{fs::PermissionsExt, process::CommandExt},
	process::{Command, ExitStatus, Stdio},
	sync::OnceLock,
};

use phf::phf_map;

macro_rules! wprint {
	($($tt:tt)*) => {
		println!("{WRAPPER_TAG} {}", format_args!($($tt)*))
	};
}
macro_rules! quick_command_execute {
	($f:expr; $([ $($item:expr),+ $(,)? ]),+ $(,)?) => {
		$(
			$f(|cmd| cmd $( .arg($item) )+ .status())?;
		)+
	};
}

const WRAPPER_TAG: &'static str = "apt-wrapper: ";

const MAX_QCOMMAND_SIZE: usize = 5;
const QUICK_COMMAND_TABLE: phf::Map<&'static str, QuickCommand> = phf_map! {
	"H" => QuickCommand {
		description: "Help",
		callback: qcmd_help
	},
	"V" => QuickCommand {
		description: "Version",
		callback: qcmd_version
	},
	"U" => QuickCommand {
		description: "Update and Upgrade",
		callback: qcmd_fully_update
	},
	"C" => QuickCommand {
		description: "Clean and Purge",
		callback: qcmd_fully_cleanup
	}
};
const REPLACE_SEGMENT_TABLE: phf::Map<usize, phf::Map<&'static str, &'static str>> = phf_map! {
	0usize => phf_map! {
		"i" => "install",
		"r" => "remove",
		"ri" => "reinstall",
		"s" => "search",
		"u" => "update",
		"ug" => "upgrade",
		"p" => "purge",
		"c" => "clean",
		"ac" => "autoclean",
		"ap" => "autopurge",
	}
};

type QuickCommandCallbackResult = Result<QuickCommandAction, QuickCommandError>;
type QuickCommandCallback = fn() -> QuickCommandCallbackResult;

#[derive(Debug)]
enum QuickCommandAction
{
	Exit(i32),
}

#[derive(Debug)]
enum QuickCommandError
{
	IoError(std::io::Error),
	ExitedWithErrorCode(i32),
}

impl std::error::Error for QuickCommandError {}
impl std::fmt::Display for QuickCommandError
{
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
	{
		match self
		{
			Self::IoError(e) => write!(f, "Command error: {e}"),
			Self::ExitedWithErrorCode(code) => write!(f, "Command exited with error code: {code}"),
		}
	}
}

struct QuickCommand
{
	description: &'static str,
	callback: QuickCommandCallback,
}

fn backend_get_binary() -> &'static str
{
	static BINARY: OnceLock<&'static str> = OnceLock::new();

	const BINARY_PATHS: &[&'static str] = &[
		"/usr/bin/apt",
		"/sbin/apt",
		"/usr/sbin/apt",
		"/data/data/com.termux/files/usr/bin/apt",
	];

	BINARY.get_or_init(|| {
		for p in BINARY_PATHS
		{
			let Ok(metadata) = std::fs::metadata(p)
			else
			{
				continue;
			};
			if metadata.permissions().mode() & 0o111 != 0
			{
				return p;
			}
		}

		return BINARY_PATHS[0];
	})
}
fn backend_execute_common<F, R>(f: F) -> R
where
	F: FnOnce(&mut Command) -> R,
{
	f(Command::new(backend_get_binary())
		.stdin(Stdio::inherit())
		.stdout(Stdio::inherit())
		.stderr(Stdio::inherit()))
}
fn backend_execute_common_checked<F>(f: F) -> Result<(), QuickCommandError>
where
	F: FnOnce(&mut Command) -> std::io::Result<ExitStatus>,
{
	let ret = backend_execute_common(f).map_err(|e| QuickCommandError::IoError(e))?;

	if ret.success()
	{
		Ok(())
	}
	else
	{
		Err(QuickCommandError::ExitedWithErrorCode(
			ret.code().unwrap_or(1),
		))
	}
}

fn qcommand_resolve(key: &str) -> Option<&QuickCommand>
{
	QUICK_COMMAND_TABLE.get(key)
}
fn qcommand_execute(cmd: &str, qcmd: &QuickCommand) -> Option<Result<i32, i32>>
{
	match (qcmd.callback)()
	{
		Ok(act) => match act
		{
			QuickCommandAction::Exit(code) =>
			{
				return Some(Ok(code));
			}
		},
		Err(e) =>
		{
			let code = match e
			{
				QuickCommandError::IoError(e) =>
				{
					wprint!("{cmd}: {e}");
					e.raw_os_error().unwrap_or(1)
				}
				QuickCommandError::ExitedWithErrorCode(code) =>
				{
					wprint!("{cmd}: Exited with error code {code}");
					code
				}
			};

			return Some(Err(code));
		}
	}
}
fn qcommand_seq_check(seq: &str) -> Result<(), char>
{
	// FIXME: qcommand_seq_check_req(seq: &str, f: F(&[&QuickCommand, ...])) -> ...

	for ch in seq.chars()
	{
		let mut buf = [0u8; 4];
		let cmd = ch.encode_utf8(&mut buf);

		if qcommand_resolve(cmd).is_none()
		{
			return Err(ch);
		}
	}

	Ok(())
}
fn qcommand_execute_common(mut cmd: &str) -> Option<i32>
{
	if cmd.starts_with('+')
	{
		cmd = &cmd[1..];
		if let Err(ch) = qcommand_seq_check(cmd)
		{
			wprint!("Invalid quick command(s) in '{cmd}': {ch}");
			return Some(1);
		}

		for ch in cmd.chars()
		{
			let mut buf = [0u8; 4];
			let cmd = ch.encode_utf8(&mut buf);

			let Some(qcmd) = qcommand_resolve(cmd)
			else
			{
				unreachable!();
			};

			match qcommand_execute(cmd, qcmd)
			{
				Some(Ok(_)) | None =>
				{}
				Some(Err(it)) => return Some(it),
			}
		}

		return Some(0);
	}
	else
	{
		if let Some(qcmd) = qcommand_resolve(cmd)
		{
			return match qcommand_execute(cmd, qcmd)
			{
				Some(Ok(it)) => Some(it),
				Some(Err(it)) => Some(it),
				None => None,
			};
		}

		if cmd.len() <= 1
		{
			return None;
		}
		if qcommand_seq_check(cmd).is_err()
		{
			return None;
		}

		use std::fmt::Write;

		let mut hint = String::with_capacity(cmd.len() * 3);
		for ch in cmd.chars()
		{
			write!(hint, "{ch}, ").ok();
		}
		hint.truncate(hint.len().saturating_sub(2));

		wprint!(
			"'{cmd}' is not a registered command; did you mean {hint}? use +{cmd} to run them in sequence."
		);

		return Some(1);
	}
}

fn qcmd_help() -> QuickCommandCallbackResult
{
	use std::io::Write;

	let stdout = std::io::stdout();
	let mut guard = stdout.lock();

	writeln!(guard, "Quick Commands").ok();
	writeln!(guard).ok();

	for (cmd, qcmd) in &QUICK_COMMAND_TABLE
	{
		writeln!(guard, "{cmd}: {}", qcmd.description).ok();
	}

	Ok(QuickCommandAction::Exit(0))
}
fn qcmd_version() -> QuickCommandCallbackResult
{
	quick_command_execute!(backend_execute_common_checked; ["--version"]);

	Ok(QuickCommandAction::Exit(0))
}
fn qcmd_fully_update() -> QuickCommandCallbackResult
{
	quick_command_execute!(backend_execute_common_checked; ["update"], ["upgrade", "-y"]);

	Ok(QuickCommandAction::Exit(0))
}
fn qcmd_fully_cleanup() -> QuickCommandCallbackResult
{
	quick_command_execute!(backend_execute_common_checked; ["autoclean"], ["autopurge"]);

	Ok(QuickCommandAction::Exit(0))
}

fn main()
{
	const {
		assert!(MAX_QCOMMAND_SIZE > 0);
	}

	let mut args = std::env::args().skip(1).peekable();

	if let Some(act) = args.peek()
	{
		// NOTE: Consider warning if necessary
		if act.len() <= MAX_QCOMMAND_SIZE
			&& let Some(code) = qcommand_execute_common(act)
		{
			std::process::exit(code);
		}
	}

	let e = backend_execute_common(|cmd| {
		cmd
			.args(args.enumerate().map(|(idx, p)| {
				match REPLACE_SEGMENT_TABLE.get(&idx).and_then(|tab| tab.get(&p))
				{
					Some(s) => Cow::Borrowed(OsStr::new(s)),
					None => Cow::Owned(p.into()),
				}
			}))
			.exec()
	});
	wprint!("{e}");

	std::process::exit(e.raw_os_error().unwrap_or(1));
}
