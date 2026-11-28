//! Local source-only signed skill registry administration; no network, grants or hooks.
use std::io::Read;
use std::path::PathBuf;
use taskloom::runtime::skills::*;

fn read(path: &str, maximum: u64) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err("input exceeds CLI byte limit".into());
    }
    Ok(bytes)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let output=match args.first().map(String::as_str){
        Some("inspect-archive") if args.len()==2=>serde_json::to_value(read_skill_archive(&read(&args[1],8*1024*1024)?)?.reference()?)?,
        Some("inspect") if args.len()==2=>serde_json::to_value(FileSkillRegistry::new(&args[1])?.snapshot().await?)?,
        Some("trust") if args.len()==4=>{
            let trust:SkillRegistryTrust=serde_json::from_slice(&read(&args[2],1024*1024)?)?;
            serde_json::to_value(FileSkillRegistry::new(&args[1])?.set_trust(trust,args[3].parse()?).await?)?
        }
        Some("install") if args.len()==8=>{
            let registry=FileSkillRegistry::new(&args[1])?;let store=FileSkillStore::new(&args[2]);
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
            serde_json::to_value(registry.install_from_directory(&store,RegistryInstallRequest{directory:PathBuf::from(&args[3]),metadata_file:args[4].clone(),archive_file:args[5].clone(),expected_registry_revision:args[6].parse()?,expected_catalog_revision:args[7].parse()?,now_unix:now}).await?)?
        }
        _=>return Err("usage: skill_registry inspect-archive ARCHIVE | inspect ABS_REGISTRY | trust ABS_REGISTRY TRUST_JSON EXPECTED_REG_REV | install ABS_REGISTRY SKILL_STORE ABS_SOURCE_DIR METADATA_FILE ARCHIVE_FILE EXPECTED_REG_REV EXPECTED_CATALOG_REV".into()),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
