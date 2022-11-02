use anyhow::Context;

use sentinel::oci::oci_main;

fn main() -> anyhow::Result<()> {
    // std::fs::File::create("log.json").expect("failed to create log.json");
    oci_main().context("oci_main failed")?;
    logger::debug!("oci_main done");
    Ok(())
}
