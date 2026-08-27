use super::Error;

use crate::built_info;
use crate::shared::{PRODUCT_NAME, builtin_plugins_dir, config_dir};

use std::fs::{File, read_dir};
use std::io::Write;
use std::path::{Path, PathBuf};

use path_slash::PathExt;
use zip::{ZipWriter, write::FileOptions};

pub async fn get_settings() -> crate::store::Settings {
	crate::store::get_settings().value
}

pub async fn set_settings(settings: crate::store::Settings) -> Result<(), Error> {
	// TODO(follow-up): autostart isn't wired up yet - see PRD milestone-2 plan's deferred bucket.

	crate::events::outbound::devices::set_brightness(settings.brightness).await?;
	crate::device_sleep::update_sleep_timeout_minutes(settings.sleep_timeout_minutes).await?;
	crate::device_sleep::update_sleep_when_computer_locked(settings.sleep_when_computer_locked).await?;

	let mut store = crate::store::SETTINGS_MUT.lock().await;
	store.value = settings;
	store.save()?;
	Ok(())
}

pub fn open_config_directory() -> Result<(), Error> {
	if let Err(error) = open::that_detached(config_dir()) {
		return Err(anyhow::Error::from(error).into());
	}
	Ok(())
}

pub fn open_log_directory() -> Result<(), Error> {
	if let Err(error) = open::that_detached(crate::shared::log_dir()) {
		return Err(anyhow::Error::from(error).into());
	}
	Ok(())
}

pub fn get_build_info() -> String {
	format!(
		r#"
		<details>
			<summary> {} v{} ({}) on {} </summary>
			{}
		</details>
		"#,
		PRODUCT_NAME,
		built_info::PKG_VERSION,
		built_info::GIT_COMMIT_HASH_SHORT.unwrap_or("commit hash unknown"),
		built_info::TARGET,
		built_info::DIRECT_DEPENDENCIES_STR
	)
}

fn add_dir_to_zip<W: Write + std::io::Seek>(zip: &mut ZipWriter<W>, base_dir: &Path, current_dir: &Path, options: FileOptions<()>, skip_paths: &[PathBuf]) -> std::io::Result<()> {
	for entry in read_dir(current_dir)? {
		let entry = entry?;
		let path = entry.path();
		if skip_paths.contains(&path) {
			continue;
		}
		let relative = path.strip_prefix(base_dir).unwrap();

		if path.is_dir() {
			zip.add_directory(relative.to_slash_lossy(), options)?;
			add_dir_to_zip(zip, base_dir, &path, options, skip_paths)?;
		} else {
			zip.start_file(relative.to_slash_lossy(), options)?;
			let mut file = File::open(&path)?;
			std::io::copy(&mut file, zip)?;
		}
	}
	Ok(())
}

pub async fn backup_config_directory() -> Result<bool, Error> {
	let filename = format!(
		"{}_config_{}_{}_{}.zip",
		PRODUCT_NAME,
		std::env::consts::OS,
		std::env::consts::ARCH.replace("_", "-"),
		chrono::Local::now().format("%Y%m%d")
	);

	let path = rfd::FileDialog::new()
		.set_file_name(filename)
		.add_filter(format!("{} config backup", PRODUCT_NAME), &["zip"])
		.save_file();

	let Some(path) = path else {
		return Ok(false);
	};
	let _ = std::fs::remove_file(&path);

	let temp_path = path.with_extension("zip.part");
	let file = File::create(&temp_path)?;

	let mut skip_paths = vec![temp_path.clone()];
	let config_dir = config_dir();

	if let Ok(builtin_plugins) = read_dir(builtin_plugins_dir()) {
		skip_paths.extend(builtin_plugins.flatten().map(|x| config_dir.join("plugins").join(x.file_name())));
	}

	let mut zip = ZipWriter::new(file);
	let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
	add_dir_to_zip(&mut zip, &config_dir, &config_dir, options, &skip_paths)?;
	zip.finish().map_err(anyhow::Error::from)?;

	std::fs::rename(temp_path, path)?;

	Ok(true)
}

pub async fn restore_config_directory() -> Result<(), Error> {
	let path = rfd::FileDialog::new().add_filter(format!("{} config backup", PRODUCT_NAME), &["zip"]).pick_file();

	let Some(path) = path else {
		return Ok(());
	};

	let config_dir = config_dir();
	let temp_dir = config_dir.with_extension("temp");
	let backup_dir = config_dir.with_extension("bak");
	let _ = std::fs::remove_dir_all(&temp_dir);
	let _ = std::fs::remove_dir_all(&backup_dir);

	crate::zip_extract::extract(File::open(path)?, &temp_dir).map_err(anyhow::Error::from)?;
	std::fs::rename(&config_dir, &backup_dir)?;
	std::fs::rename(temp_dir, &config_dir)?;
	let _ = std::fs::remove_dir_all(backup_dir);

	crate::restart_app();
}
