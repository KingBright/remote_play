//! Create a fresh local test node in the user's already-authorized group.
//! Does not launch networking, change the source profile, or print an invite/key.
use remote_core::mesh::{AppPrivateMeshConfigStore,MeshConfig};
use std::{path::PathBuf,error::Error};
fn main()->Result<(),Box<dyn Error+Send+Sync>> {
    let mut args=std::env::args().skip(1);
    let source=PathBuf::from(args.next().ok_or("source profile path required")?);
    let destination=PathBuf::from(args.next().ok_or("fresh test profile path required")?);
    if args.next().is_some() || !source.is_absolute() || !destination.is_absolute() || destination.exists(){return Err("Use one existing absolute profile and a fresh absolute test destination".into());}
    let parent=destination.parent().ok_or("destination has no parent")?;
    if !parent.is_dir()||parent.is_symlink(){return Err("test parent must already exist and be a regular directory".into());}
    let existing=AppPrivateMeshConfigStore::new(source).load()?.ok_or("source group not configured")?;
    let test=MeshConfig::from_invite_code(&existing.invite_code(),"RemotePlay bounded native acceptance")?;
    if test.node_id==existing.node_id{return Err("test must not duplicate the production identity".into());}
    AppPrivateMeshConfigStore::new(&destination).save(&test)?;
    println!("NATIVE_TEST_NODE {}",serde_json::json!({"node_id":test.node_id,"destination":destination,"production_identity_changed":false,"network_started":false}));
    Ok(())
}
