//! Bounded opt-in sequence validation. No network keys, implicit desktop source,
//! capture permissions or input injection are managed here. The real restored UI
//! supplies connection, source-selection, frame and shutdown observations.
use serde::Deserialize;
use std::time::{Duration,Instant};
#[derive(Clone,Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub device:String,pub window:u32,pub pid:i32,pub title:String,
}
#[derive(Clone,Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub sources:Vec<Source>,pub route:String,pub hold_seconds:u64,pub minimum_frames:u64,
}
pub(super) struct Sequence {
    pub plan:Plan,pub index:usize,pub phase_started:Instant,pub healthy_since:Option<Instant>,
    pub receipts:Vec<serde_json::Value>,pub error:Option<String>,pub closing:Option<Instant>,
}
impl Plan {
    pub fn parse(text:&str)->Result<Self,String> {
        if text.len()>16*1024{return Err("test plan too large".into());}
        let p:Self=serde_json::from_str(text).map_err(|e|e.to_string())?;
        if !(2..=6).contains(&p.sources.len()) || !(2..=10).contains(&p.hold_seconds) || !(15..=240).contains(&p.minimum_frames) || !["LAN","P2P","Relay"].contains(&p.route.as_str()) {return Err("invalid bounded test sequence limits".into());}
        for s in &p.sources {
            if s.device.len()!=32||!s.device.bytes().all(|c|c.is_ascii_hexdigit())||s.window==0||s.pid<=0||!s.title.starts_with("RemotePlay Native Decode Acceptance ")||s.title.len()>160 {return Err("sequence source must explicitly identify an owned synthetic window".into());}
        }
        if !p.sources.windows(2).any(|s|s[0].device!=s[1].device){return Err("sequence must exercise more than one target device".into());}
        Ok(p)
    }
}
impl Sequence {
    pub fn from_env()->Option<Result<Self,String>> {
        if std::env::var_os("REMOTE_PLAY_RESTORED_TEST_OUTPUT").is_none(){return None;}
        std::env::var("REMOTE_PLAY_RESTORED_TEST_SEQUENCE").ok().map(|text|Plan::parse(&text).map(|plan|Self{plan,index:0,phase_started:Instant::now(),healthy_since:None,receipts:Vec::new(),error:None,closing:None}))
    }
    pub fn step(&self)->&Source{&self.plan.sources[self.index]}
    pub fn phase_expired(&self)->bool{self.phase_started.elapsed()>Duration::from_secs(25)}
}
#[cfg(test)]mod tests {
    use super::*;
    fn plan()->String{serde_json::json!({"sources":[{"device":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","window":5,"pid":1,"title":"RemotePlay Native Decode Acceptance A"},{"device":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","window":7,"pid":2,"title":"RemotePlay Native Decode Acceptance B"}],"route":"Relay","hold_seconds":3,"minimum_frames":30}).to_string()}
    #[test]fn valid_source_sequence_is_strict_and_bounded(){assert_eq!(Plan::parse(&plan()).unwrap().sources.len(),2);}
    #[test]fn wrong_window_unbounded_wait_and_unknown_fields_are_rejected(){
        let p:serde_json::Value=serde_json::from_str(&plan()).unwrap();
        for key in ["hold_seconds","minimum_frames"] {let mut v=p.clone();v[key]=9999.into();assert!(Plan::parse(&v.to_string()).is_err());}
        let mut v=p.clone();v["sources"][0]["window"]=0.into();assert!(Plan::parse(&v.to_string()).is_err());
        let mut v=p.clone();v["sources"][0]["title"]="Private document".into();assert!(Plan::parse(&v.to_string()).is_err());
        let mut v=p;v["enable_input"]=true.into();assert!(Plan::parse(&v.to_string()).is_err());
    }
    #[test]fn same_device_only_does_not_claim_switch_coverage(){let mut p:serde_json::Value=serde_json::from_str(&plan()).unwrap();p["sources"][1]["device"]=p["sources"][0]["device"].clone();assert!(Plan::parse(&p.to_string()).is_err());}
}
