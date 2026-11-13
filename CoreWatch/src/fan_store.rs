//! Persist only applied fan modes, matched by sensor ID and label on discovery.
use std::{collections::HashMap, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use crate::fans::{Mode, Profile};

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedControl { pub name: String, pub mode: Mode }
#[derive(Default, Serialize, Deserialize)]
pub struct FanStore { pub controls: HashMap<String, SavedControl> }
impl FanStore {
    pub fn path() -> Result<PathBuf,String> {
        Ok(PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("Windows settings folder unavailable")?).join("CoreWatch").join("fan-profiles.json"))
    }
    pub fn load(path: &Path) -> Result<Self,String> {
        match std::fs::read(path) {
            Ok(data) => {
                let mut saved:Self=serde_json::from_slice(&data).map_err(|e|format!("Could not read saved fan profiles: {e}"))?;
                if saved.controls.values().any(|s| !valid_mode(&s.mode)) { return Err("Saved fan profiles contain invalid curve values".into()); }
                saved.controls.retain(|_,s| !matches!(s.mode,Mode::Automatic));
                Ok(saved)
            }
            Err(e) if e.kind()==std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e)=>Err(format!("Could not read saved fan profiles: {e}")),
        }
    }
    pub fn remember(&mut self,id:&str,name:&str,mode:&Mode) {
        if matches!(mode,Mode::Automatic) {self.controls.remove(id);}
        else if valid_mode(mode) {self.controls.insert(id.into(),SavedControl{name:name.into(),mode:mode.clone()});}
    }
    pub fn matching(&self,id:&str,name:&str)->Option<Mode> {
        self.controls.get(id).filter(|s|s.name==name).map(|s|s.mode.clone())
    }
    pub fn save(&self,path:&Path)->Result<(),String> {
        let parent=path.parent().ok_or("Missing settings folder")?;
        std::fs::create_dir_all(parent).map_err(|e|e.to_string())?;
        let tmp=path.with_extension("json.tmp");
        let data=serde_json::to_vec_pretty(self).map_err(|e|e.to_string())?;
        use std::io::Write;
        let mut file=std::fs::File::create(&tmp).map_err(|e|e.to_string())?;
        file.write_all(&data).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;
        drop(file);
        #[cfg(windows)] {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{MoveFileExW,MOVEFILE_REPLACE_EXISTING,MOVEFILE_WRITE_THROUGH};
            let from:Vec<u16>=tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe{MoveFileExW(from.as_ptr(),to.as_ptr(),MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH)}==0 {return Err(std::io::Error::last_os_error().to_string());}
        }
        #[cfg(not(windows))] std::fs::rename(tmp,path).map_err(|e|e.to_string())?;
        Ok(())
    }
}
pub fn valid_mode(mode:&Mode)->bool {
    match mode {
        Mode::Automatic=>true,
        Mode::Manual(v)=>v.is_finite() && (35.0..=100.0).contains(v),
        Mode::Curve{profile,points,..}=> {
            let valid=points.iter().all(|(t,d)|t.is_finite() && (0.0..=120.0).contains(t) && d.is_finite() && (35.0..=100.0).contains(d)) && points.windows(2).all(|p|p[0].0<p[1].0);
            valid && (*profile==Profile::Custom || *points==profile.points())
        }
    }
}
#[cfg(test)] mod tests {
    use super::*; use crate::fans::Source;
    #[test] fn applied_curve_survives_restart_and_reset_clears_it() {
        let dir=std::env::temp_dir().join(format!("corewatch-fans-{}",std::process::id()));let path=dir.join("fans.json");
        let mut store=FanStore::default();
        let mode=Mode::Curve{profile:Profile::Custom,source:Source::GpuMax,points:[(30.,35.),(50.,60.),(70.,80.),(90.,100.)]};
        store.remember("/fan/1","CPU Fan",&mode);store.save(&path).unwrap();
        let mut loaded=FanStore::load(&path).unwrap();assert_eq!(loaded.matching("/fan/1","CPU Fan"),Some(mode));
        assert!(loaded.matching("/fan/1","Pump").is_none());assert!(loaded.matching("/fan/2","CPU Fan").is_none());
        loaded.remember("/fan/1","CPU Fan",&Mode::Automatic);loaded.save(&path).unwrap();assert!(FanStore::load(&path).unwrap().controls.is_empty());
        std::fs::write(&path,b"bad json").unwrap();assert!(FanStore::load(&path).is_err());std::fs::remove_dir_all(dir).unwrap();
    }
    #[test] fn rejects_invalid_saved_curves() {
        assert!(!valid_mode(&Mode::Manual(f32::NAN)));
        assert!(!valid_mode(&Mode::Curve{profile:Profile::Custom,source:Source::Cpu,points:[(50.,40.),(30.,50.),(70.,80.),(90.,100.)]}));
    }
}
