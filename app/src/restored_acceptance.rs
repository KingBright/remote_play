//! Bounded opt-in sequence validation. No network keys, implicit desktop source,
//! capture permissions or input injection are managed here. The real restored UI
//! supplies connection, source-selection, frame and shutdown observations.
use serde::{Deserialize, Serialize};
use protocol::session::{CaptureSource, CaptureSourceInfo};
use std::time::{Duration,Instant};

/// Explicit selector for the bounded entry. IDs are the product's protocol IDs;
/// the returned choice is an actual catalog item, never an invented desktop.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceRequest {
    pub source: CaptureSource,
    pub title: String,
    #[serde(default)]
    pub process_id: Option<i32>,
}
impl SourceRequest {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 1024 { return Err("source selector too large".into()); }
        serde_json::from_str::<Self>(text).map_err(|e| e.to_string())?.validate()
    }
    fn validate(self) -> Result<Self, String> {
        if self.title.is_empty() || self.title.len() > 256 || self.title.contains('\0') {
            return Err("source selector requires an exact bounded title".into());
        }
        match self.source {
            CaptureSource::Window(id) if id > 0 && self.process_id.is_some_and(|pid| pid > 0) => {},
            CaptureSource::MainDisplay if self.process_id.is_none() => {},
            CaptureSource::Display(id) if id > 0 && self.process_id.is_none() => {},
            _ => return Err("source selector has an invalid ID/process binding".into()),
        }
        Ok(self)
    }
    pub fn select(&self, catalog: &[CaptureSourceInfo]) -> Option<CaptureSourceInfo> {
        let source = remote_core::view_commands::enumerated_source(catalog, self.source)?;
        (source.title == self.title && source.process_id == self.process_id).then(|| source.clone())
    }
    pub fn from_env() -> Option<Result<Self, String>> {
        if std::env::var_os("REMOTE_PLAY_RESTORED_TEST_OUTPUT").is_none() { return None; }
        let legacy = ["REMOTE_PLAY_RESTORED_TEST_TITLE", "REMOTE_PLAY_RESTORED_TEST_WINDOW", "REMOTE_PLAY_RESTORED_TEST_PID"];
        if let Some(raw) = std::env::var_os("REMOTE_PLAY_RESTORED_TEST_SOURCE") {
            if legacy.iter().any(|name| std::env::var_os(name).is_some()) {
                return Some(Err("use a JSON source selector or legacy window arguments, not both".into()));
            }
            return Some(raw.to_str().ok_or_else(|| "source selector is not UTF-8".into()).and_then(Self::parse));
        }
        if !legacy.iter().any(|name| std::env::var_os(name).is_some()) { return None; }
        Some((|| {
            let title = std::env::var(legacy[0]).map_err(|_| "legacy selector requires title")?;
            let id = std::env::var(legacy[1]).ok().and_then(|v| v.parse().ok()).ok_or("legacy selector requires window ID")?;
            let pid = std::env::var(legacy[2]).ok().and_then(|v| v.parse().ok()).ok_or("legacy selector requires process ID")?;
            Self { source: CaptureSource::Window(id), title, process_id: Some(pid) }.validate()
        })())
    }
}
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
    fn item(source: CaptureSource, title: &str, pid: Option<i32>) -> CaptureSourceInfo {
        CaptureSourceInfo { source, title: title.into(), application: String::new(), process_id: pid,
            width: 0, height: 0, supports_input: false }
    }
    #[test] fn bounded_selector_uses_real_linux_main_display_and_never_falls_back() {
        let request = SourceRequest::parse(r#"{"source":"MainDisplay","title":"Desktop"}"#).unwrap();
        assert!(request.select(&[]).is_none());
        assert!(request.select(&[item(CaptureSource::Window(7), "Desktop", Some(8))]).is_none());
        let desktop = item(CaptureSource::MainDisplay, "Desktop", None);
        assert_eq!(request.select(&[desktop.clone()]), Some(desktop));
    }
    #[test] fn bounded_selector_preserves_display_window_and_process_identity() {
        let request = SourceRequest::parse(r#"{"source":{"Window":7},"title":"Window","process_id":8}"#).unwrap();
        for wrong in [item(CaptureSource::Display(7), "Window", None),
            item(CaptureSource::Window(7), "Window", Some(9)), item(CaptureSource::Window(7), "Changed", Some(8))] {
            assert!(request.select(&[wrong]).is_none());
        }
        assert!(request.select(&[item(CaptureSource::MainDisplay, "Desktop", None)]).is_none());
        assert!(SourceRequest::parse(r#"{"source":{"Display":7},"title":"External"}"#).unwrap()
            .select(&[item(CaptureSource::Display(7), "External", None)]).is_some());
    }
    #[test] fn bounded_selector_rejects_ambiguity_invalid_ids_and_control_extensions() {
        let request = SourceRequest::parse(r#"{"source":"MainDisplay","title":"Desktop"}"#).unwrap();
        let desktop = item(CaptureSource::MainDisplay, "Desktop", None);
        assert!(request.select(&[desktop.clone(), desktop]).is_none());
        for text in [r#"{"source":{"Window":0},"title":"Window","process_id":8}"#,
            r#"{"source":{"Window":7},"title":"Window"}"#,
            r#"{"source":"MainDisplay","title":"Desktop","process_id":8}"#,
            r#"{"source":"MainDisplay","title":"Desktop","enable_input":true}"#] {
            assert!(SourceRequest::parse(text).is_err());
        }
    }
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
