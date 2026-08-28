//! Pages, built on profiles.
//!
//! A page *is* a profile: each is already its own file on disk, and
//! [`set_selected_profile`](crate::events::frontend::profiles::set_selected_profile) already
//! performs the whole swap - `willDisappear` for the outgoing instances, a screen clear,
//! `willAppear` for the incoming ones, then persisting the selection. This module only adds an
//! ordering rule and the step/add/remove operations on top.

use crate::events::frontend::profiles;
use crate::store::profiles::{DEVICE_STORES, get_device_profiles};

use anyhow::Result;

/// The first page keeps the name profiles have always defaulted to.
const FIRST_PAGE: &str = "Default";

/// Sort key for a page: the default page first, then by trailing number so `Page 10` follows
/// `Page 9` rather than `Page 1`. `get_device_profiles` returns raw directory order, which is not
/// stable, so pages would otherwise shuffle between runs.
fn order_key(page: &str) -> (u8, u64, String) {
	if page == FIRST_PAGE {
		return (0, 0, String::new());
	}
	let trailing: String = page.chars().rev().take_while(char::is_ascii_digit).collect();
	let number = trailing.chars().rev().collect::<String>().parse().unwrap_or(u64::MAX);
	(1, number, page.to_owned())
}

/// Every page on a device, in display order.
pub fn list(device: &str) -> Vec<String> {
	let mut pages = get_device_profiles(device).unwrap_or_default();
	pages.sort_by_key(|page| order_key(page));
	pages
}

/// The page currently shown, falling back to the first if the device has no selection yet.
pub async fn current(device: &str) -> String {
	DEVICE_STORES
		.write()
		.await
		.get_selected_profile(device)
		.unwrap_or_else(|_| FIRST_PAGE.to_owned())
}

/// Move `delta` pages from the current one, wrapping at both ends.
pub async fn step(device: &str, delta: i32) -> Result<()> {
	let pages = list(device);
	if pages.len() < 2 {
		return Ok(());
	}

	let current = current(device).await;
	let index = pages.iter().position(|page| *page == current).unwrap_or(0) as i32;
	let next = (index + delta).rem_euclid(pages.len() as i32) as usize;

	show(device, &pages[next]).await
}

/// Switch to a page by name, creating it if it does not exist yet.
pub async fn show(device: &str, page: &str) -> Result<()> {
	profiles::set_selected_profile(device.to_owned(), page.to_owned()).await?;
	crate::frontend_events::emit(crate::frontend_events::FrontendEvent::SwitchProfile);
	Ok(())
}

/// Add a page and switch to it. The store is created on demand by `get_profile_store_mut`, so
/// selecting an unused name is all it takes.
pub async fn add(device: &str) -> Result<String> {
	let pages = list(device);
	let mut number = pages.len() + 1;
	let name = loop {
		let candidate = format!("Page {number}");
		if !pages.contains(&candidate) {
			break candidate;
		}
		number += 1;
	};

	show(device, &name).await?;
	Ok(name)
}

/// Delete a page, moving to a neighbouring one first. Refused when it is the only page left -
/// a device with no pages has nowhere to draw.
pub async fn remove(device: &str, page: &str) -> Result<()> {
	let pages = list(device);
	if pages.len() < 2 {
		return Ok(());
	}

	if current(device).await == page {
		step(device, -1).await?;
	}
	profiles::delete_profile(device.to_owned(), page.to_owned()).await;
	Ok(())
}
