//! Checking GitHub for a newer rook, and fetching it.

use anyhow::Result;
use rook_core::upgrade;

pub(crate) fn cmd_update(check_only: bool, force: bool, rollback: bool, json: bool) -> Result<()> {
    // Nothing is asked of the network to go back, which is most of the point:
    // the version to go back to is already on the disk, and a rollback that
    // needed GitHub to be reachable would be unavailable exactly when an
    // update has gone wrong.
    if rollback {
        let layout = upgrade::Layout::here().map_err(anyhow::Error::msg)?;
        let done = upgrade::rollback(&layout).map_err(anyhow::Error::msg)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&done)?);
            return Ok(());
        }
        for (at, now_kept) in &done.back {
            println!("  {} (what was there is now {})", at.display(), now_kept.display());
        }
        for why in &done.left {
            println!("  — {why}");
        }
        println!("\nrun this again to undo it. `rook --version` says which one is there now.");
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        // No store and no workspace: this replaces files beside the binary and
        // has nothing to say about a session. Opening one would mean `rook
        // update` refusing while `rookd` holds the lock, which is most of the
        // time somebody thinks to run it.
        let config = rook_core::Config::load()?;
        let proxy = config.proxy.for_install();
        let found = upgrade::check(&proxy).await.map_err(anyhow::Error::msg)?;

        if check_only {
            match json {
                true => println!("{}", serde_json::to_string_pretty(&found)?),
                false => say_what_is_published(&found),
            }
            return anyhow::Ok(());
        }
        if !found.newer && !force {
            match json {
                true => println!("{}", serde_json::to_string_pretty(&found)?),
                false => {
                    say_what_is_published(&found);
                    println!("\nnothing to do. `--force` fetches it anyway.");
                }
            }
            return anyhow::Ok(());
        }

        let layout = upgrade::Layout::here().map_err(anyhow::Error::msg)?;
        if !json {
            say_what_is_published(&found);
            println!("\nfetching into {} …", layout.bin.display());
        }
        let done = upgrade::apply(&found, &layout, &proxy).await.map_err(anyhow::Error::msg)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&done)?);
            return anyhow::Ok(());
        }
        println!("{} → {}", done.from, done.to);
        println!("verified: {}", done.verified);
        for (at, kept) in &done.replaced {
            println!("  {} (the previous one is {})", at.display(), kept.display());
        }
        for why in &done.left {
            println!("  — {why}");
        }
        println!("\n{}", done.restart);
        anyhow::Ok(())
    })
}

/// The two versions and where to read what changed, which is what somebody
/// needs before agreeing to replace the binary they are running.
fn say_what_is_published(found: &upgrade::Check) {
    println!("running:   {}", found.running);
    println!("published: {} ({})", found.latest, found.tag);
    if !found.notes.is_empty() {
        println!("notes:     {}", found.notes);
    }
    match (&found.asset, found.newer) {
        (None, _) => println!(
            "\nrelease {} carries nothing named for {}. {}",
            found.tag,
            found.target,
            upgrade::BUILD_IT_INSTEAD
        ),
        (Some(asset), true) => {
            println!("\n{} is newer. {} is {} bytes.", found.latest, asset.name, asset.size)
        }
        // A build from the repository is ahead of the last release, which is
        // not being up to date and is not being behind either.
        (Some(_), false) if found.running != found.latest => {
            println!("\n{} is what is running, and it is not behind {}.", found.running, found.latest)
        }
        (Some(_), false) => println!("\nup to date."),
    }
}
