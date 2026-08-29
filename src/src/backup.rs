//! Backing the whole configuration up to a zip, and restoring it back.
//!
//! A backup carries everything the user built: their action library, the artwork composited for it,
//! every device's dials and pages, and the app's settings. Restoring is a *replacement*, not a
//! merge - whatever the archive holds becomes the entire configuration.
//!
//! The tree being replaced is never deleted. It is moved aside, so restoring the wrong file is one
//! `mv` away from being undone rather than being the end of someone's setup.

use crate::shared::config_dir;

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// What a backup carries, relative to the configuration root.
///
/// `logs/` is deliberately absent - it is noise, it is the largest thing in the tree, and none of
/// it is configuration. So is `plugins/`, a leftover directory from the removed plugin system that
/// nothing reads any more; a backup taken now should not carry it into the future.
const CONTENTS: &[&str] = &["customs", "predefined", "profiles", "images", "settings.json"];

/// What a replaced configuration is renamed to, before its timestamp.
const REPLACED_PREFIX: &str = ".rustydeck.replaced-";

/// How many replaced configurations to keep.
///
/// Each is a full copy of everything, artwork included, so they are not small. Moving one aside is
/// there to make a mistaken restore recoverable, and that only needs the last one or two - keeping
/// every restore ever made just fills the home directory.
const KEEP_REPLACED: usize = 2;

/// Eight well-mixed characters, to stop two backups taken on the same day from colliding.
///
/// No RNG crate is in the tree and none is warranted for this: nanosecond time through a xorshift
/// is more than enough entropy to distinguish two files named on the same date.
fn random_tag() -> String {
	const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

	let mut state = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|elapsed| elapsed.as_nanos() as u64)
		// The fallback is only reachable if the clock is before 1970; any odd constant will do.
		.unwrap_or(0x9E37_79B9_7F4A_7C15)
		| 1;

	(0..8)
		.map(|_| {
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
			ALPHABET[(state % ALPHABET.len() as u64) as usize] as char
		})
		.collect()
}

/// The name to offer for a new backup: `rustydeck_DDMMYY_XXXXXXXX.zip`.
pub fn archive_name() -> String {
	format!("rustydeck_{}_{}.zip", chrono::Local::now().format("%d%m%y"), random_tag())
}

fn add_file<W: Write + Seek>(zip: &mut ZipWriter<W>, root: &Path, path: &Path, options: SimpleFileOptions) -> Result<()> {
	let name = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
	zip.start_file(name, options)?;
	let mut source = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
	std::io::copy(&mut source, zip)?;
	Ok(())
}

/// Add every file under `directory`, returning how many there were.
fn add_directory<W: Write + Seek>(zip: &mut ZipWriter<W>, root: &Path, directory: &Path, options: SimpleFileOptions) -> Result<usize> {
	let mut count = 0;
	for entry in std::fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))? {
		let path = entry?.path();
		if path.is_dir() {
			count += add_directory(zip, root, &path, options)?;
		} else if path.is_file() {
			add_file(zip, root, &path, options)?;
			count += 1;
		}
	}
	Ok(count)
}

/// Write the configuration to `destination` as a zip.
fn write_archive(destination: &Path) -> Result<()> {
	let root = config_dir();
	let file = std::fs::File::create(destination).with_context(|| format!("creating {}", destination.display()))?;
	let mut zip = ZipWriter::new(file);
	let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

	let mut written = 0;
	for entry in CONTENTS {
		let path = root.join(entry);
		if !path.exists() {
			continue;
		}
		if path.is_dir() {
			written += add_directory(&mut zip, &root, &path, options)?;
		} else {
			add_file(&mut zip, &root, &path, options)?;
			written += 1;
		}
	}
	zip.finish()?;

	if written == 0 {
		// An empty archive would restore to an empty configuration, which is worse than refusing.
		let _ = std::fs::remove_file(destination);
		bail!("there is nothing to back up yet");
	}

	Ok(())
}

/// The timestamp format a replaced configuration is stamped with.
const REPLACED_STAMP: &str = "%Y%m%d-%H%M%S";

/// Delete all but the most recent [`KEEP_REPLACED`] configurations moved aside by earlier restores.
///
/// A directory only counts if its suffix parses as one of our timestamps. Matching on the prefix
/// alone would be enough to *find* them, but this decides what to hand to `remove_dir_all`: sorting
/// names lexically lets anything that merely looks similar sort above a real one and displace it,
/// and a directory we cannot account for is not one to delete.
fn prune_replaced(parent: &Path) {
	let Ok(entries) = std::fs::read_dir(parent) else { return };

	let mut replaced: Vec<(chrono::NaiveDateTime, PathBuf)> = entries
		.flatten()
		.map(|entry| entry.path())
		.filter(|path| path.is_dir())
		.filter_map(|path| {
			let name = path.file_name()?.to_string_lossy().into_owned();
			let stamp = name.strip_prefix(REPLACED_PREFIX)?;
			let stamp = chrono::NaiveDateTime::parse_from_str(stamp, REPLACED_STAMP).ok()?;
			Some((stamp, path))
		})
		.collect();

	replaced.sort_by_key(|(stamp, _)| *stamp);

	let Some(surplus) = replaced.len().checked_sub(KEEP_REPLACED) else { return };
	for (_, path) in replaced.into_iter().take(surplus) {
		if let Err(error) = std::fs::remove_dir_all(&path) {
			log::warn!("Failed to prune {}: {error}", path.display());
		}
	}
}

/// Refuse an archive that is not one of ours, before anything on disk is moved.
fn validate<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<()> {
	let mut recognised = false;

	for index in 0..zip.len() {
		let entry = zip.by_index(index)?;
		// `enclosed_name` is `None` for an absolute path or one climbing out with `..`, which is
		// how a crafted archive would write outside the directory it is extracted into.
		let Some(name) = entry.enclosed_name() else {
			bail!("that archive contains an unsafe path, so nothing was restored");
		};

		if let Some(first) = name.components().next()
			&& CONTENTS.contains(&first.as_os_str().to_string_lossy().as_ref())
		{
			recognised = true;
		}
	}

	if !recognised {
		bail!("that does not look like a RustyDeck backup");
	}

	Ok(())
}

/// Replace the configuration with the contents of `archive`, returning where the old one went.
///
/// The swap goes through a staging directory beside the configuration root rather than extracting
/// over the live tree. Extraction is not atomic and can fail halfway through; doing it to one side
/// means a failure leaves the existing configuration untouched, and the visible change is two
/// renames of a directory that is already fully written.
fn replace_from_archive(archive: &Path) -> Result<PathBuf> {
	let root = config_dir();
	let parent = root.parent().ok_or_else(|| anyhow!("the configuration directory has no parent"))?.to_path_buf();

	let file = std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
	let mut zip = ZipArchive::new(file).context("reading the archive")?;
	validate(&mut zip)?;

	let staging = parent.join(".rustydeck.restoring");
	let _ = std::fs::remove_dir_all(&staging);
	zip.extract(&staging).context("extracting the archive")?;

	let aside = parent.join(format!("{REPLACED_PREFIX}{}", chrono::Local::now().format(REPLACED_STAMP)));
	if root.exists() {
		std::fs::rename(&root, &aside).with_context(|| format!("moving {} aside", root.display()))?;
	}

	if let Err(error) = std::fs::rename(&staging, &root) {
		// Put back what was moved aside rather than leaving the app with no configuration at all.
		if aside.exists() {
			let _ = std::fs::rename(&aside, &root);
		}
		return Err(error).context("swapping the restored configuration into place");
	}

	// A zip carries no empty directories, and several of these are empty by design - `images/` is
	// only ever a path to resolve against, and `logs/` holds nothing worth backing up. Recreate the
	// standard tree so nothing later trips over a directory that is simply missing.
	crate::shared::initialise_config_dir();

	// Only now that the new tree is in place, so a failure above never costs the older copies.
	prune_replaced(&parent);

	Ok(aside)
}

/// Back the configuration up, with the stores held still so the archive is a consistent snapshot.
pub async fn export(destination: PathBuf) -> Result<()> {
	let _locks = crate::store::profiles::acquire_locks_mut().await;
	tokio::task::spawn_blocking(move || write_archive(&destination))
		.await
		.map_err(|error| anyhow!("the backup task panicked: {error}"))?
}

/// Restore the configuration from `archive`, returning where the replaced one was moved to.
///
/// The store write locks are held across the swap, so nothing can save into the tree while it is
/// being replaced, and the in-memory stores are dropped before they are released - they hold the
/// configuration that has just been superseded, and the next save would write it straight back
/// over the restored files.
pub async fn restore(archive: PathBuf) -> Result<PathBuf> {
	let mut locks = crate::store::profiles::acquire_locks_mut().await;

	let aside = tokio::task::spawn_blocking(move || replace_from_archive(&archive))
		.await
		.map_err(|error| anyhow!("the restore task panicked: {error}"))??;

	locks.profile_stores.clear();
	locks.device_stores.clear();
	drop(locks);

	crate::store::reload_settings().await;
	// Dropping the caches is only half of it: the read paths do not create what is missing, so
	// without this every one of them fails with "profile not found" until the app is restarted.
	crate::store::profiles::prime_stores().await;

	Ok(aside)
}
