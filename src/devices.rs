use crate::{
    backends,
    files::{Problem, Result},
    jobs::{Job, Jobs, Mode},
    preview,
    processes::Process,
    projects::Project,
};
use reqwest::Url;
use serde_json::{Value, json};
use std::{collections::HashSet, net::IpAddr, sync::Arc, time::Duration};
use tokio::time::{self, Instant};

async fn detected(jobs: &Jobs, project: &Project) -> Result<Vec<Value>> {
    let sdk = jobs.flutter.require()?;
    let command = jobs
        .flutter
        .command(&sdk, ["devices", "--machine", "--device-timeout", "4"])?;
    let output = command.capture(&project.root, 512 * 1024).await?;
    if !output.status.success() {
        return Err(Problem::new(
            422,
            "Flutter could not list devices. Check Flutter Doctor in Settings.",
        ));
    }
    // Flutter can print a progress line before the machine-readable device list.
    let output = String::from_utf8_lossy(&output.stdout);
    let mut start = 0;
    let devices = output
        .split_inclusive('\n')
        .find_map(|line| {
            let index = start;
            start += line.len();
            if !line.trim_start().starts_with('[') {
                return None;
            }
            serde_json::Deserializer::from_str(&output[index..])
                .into_iter::<Vec<Value>>()
                .next()?
                .ok()
        })
        .ok_or_else(|| Problem::new(502, "Flutter returned an invalid device list."))?;
    Ok(devices
        .into_iter()
        .filter(|device| {
            device["isSupported"] == true && device["id"].as_str().is_some_and(valid_id)
        })
        .map(|device| {
            json!({
                "id":device["id"],"name":device["name"],"platform":device["targetPlatform"],
                "emulator":device["emulator"] == true,"capabilities":device["capabilities"],
            })
        })
        .collect())
}

fn valid_id(device: &str) -> bool {
    !device.starts_with('-')
        && !device.is_empty()
        && device.len() <= 240
        && device.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.' | b'[' | b']')
        })
}

pub async fn list(jobs: &Jobs, project: &Project) -> Result<Value> {
    let devices = detected(jobs, project).await?;
    Ok(json!({"devices":devices, "session":jobs.device_session(&project.id)}))
}

pub async fn start(
    jobs: &Arc<Jobs>,
    project: Project,
    device: &str,
    input: &Value,
) -> Result<Value> {
    if !valid_id(device) {
        return Err(Problem::new(400, "Choose a device listed by Flutter."));
    }
    let flags = run_flags(input)?;
    let info = detected(jobs, &project)
        .await?
        .into_iter()
        .find(|entry| entry["id"] == device)
        .ok_or_else(|| {
            Problem::new(
                404,
                "Flutter cannot find that device. Refresh the device list.",
            )
        })?;
    let target = project.clone();
    let device = device.to_owned();
    jobs.enqueue(
        "device",
        Some(project),
        Mode::Device,
        move |jobs, job| async move { run(&jobs, &job, &target, &device, &info, flags).await },
    )
}

fn run_flags(input: &Value) -> Result<Vec<String>> {
    let mut flags = Vec::new();
    let flavor = input["flavor"].as_str().unwrap_or_default().trim();
    if !flavor.is_empty() {
        if flavor.len() > 40
            || !flavor.starts_with(|c: char| c.is_ascii_alphabetic())
            || !flavor
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err(Problem::new(400, "Enter a valid Flutter flavor."));
        }
        flags.push(format!("--flavor={flavor}"));
    }
    let defines = input["defines"].as_str().unwrap_or_default();
    let mut seen = HashSet::new();
    if defines.len() > 8000 {
        return Err(Problem::new(413, "Dart defines are limited to 8 KiB."));
    }
    for line in defines
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| Problem::new(400, "Enter one NAME=value Dart define per line."))?;
        if key.is_empty()
            || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || value.chars().any(char::is_control)
            || !seen.insert(key)
            || seen.len() > 32
        {
            return Err(Problem::new(
                400,
                "Use up to 32 distinct Dart define names with letters, digits or underscores.",
            ));
        }
        flags.push(format!("--dart-define={key}={value}"));
    }
    Ok(flags)
}

fn debug_url(value: &Value) -> Option<String> {
    let url = Url::parse(value.as_str()?).ok()?;
    let host = url.host_str()?;
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    (matches!(url.scheme(), "http" | "https")
        && loopback
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}

struct Session {
    device: String,
    app: Option<String>,
    pending: Option<(u64, Instant, bool)>,
    counter: u64,
    error: Option<String>,
}

impl Session {
    fn event(&mut self, job: &Job, message: Value) -> bool {
        let params = &message["params"];
        match message["event"].as_str() {
            Some("app.start" | "app.started") => {
                if let Some(id) = params["appId"].as_str() {
                    self.app = Some(id.to_owned());
                }
                if message["event"] == "app.started" {
                    job.device_ready(&self.device);
                    job.log(format!("Flutter is running on {}.", self.device));
                }
            }
            Some(event @ ("app.devTools" | "app.debugPort")) => {
                let field = if event == "app.devTools" {
                    "uri"
                } else {
                    "devToolsUri"
                };
                if let Some(url) = debug_url(&params[field]) {
                    job.device_debug_url(url);
                }
            }
            Some("app.log" | "daemon.logMessage" | "app.debugOutput" | "app.warning") => {
                if let Some(log) = params["log"].as_str().or(params["message"].as_str()) {
                    device_log(job, log);
                }
            }
            Some("app.stop") => {
                self.error = params["error"].as_str().map(str::to_owned);
                return true;
            }
            _ => {}
        }
        if let Some((id, _, restart)) = self.pending
            && message["id"].as_u64() == Some(id)
        {
            self.pending = None;
            let result = &message["result"];
            if !message["error"].is_null() || result["code"].as_i64().is_some_and(|code| code != 0)
            {
                let detail = result["message"]
                    .as_str()
                    .or(message["error"].as_str())
                    .or(message["error"]["message"].as_str())
                    .unwrap_or("Flutter could not apply this change.");
                job.compile_error(detail.to_owned());
            } else {
                job.hot_reloaded();
                job.log(if restart {
                    "Hot restart complete."
                } else {
                    "Hot reload complete."
                });
            }
        }
        false
    }
}

fn device_log(job: &Job, line: &str) {
    if let Some((_, address)) = line.split_once("DevTools debugger and profiler is available at:")
        && let Some(url) = debug_url(&json!(address.trim()))
    {
        job.device_debug_url(url);
    }
    job.log(line);
}

async fn run(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    device: &str,
    info: &Value,
    flags: Vec<String>,
) -> Result<Value> {
    let sdk = jobs.flutter.require()?;
    jobs.pub_command(job, project, &sdk, &["pub", "get"])
        .await?;
    let platform = info["platform"].as_str().unwrap_or_default();
    if platform.starts_with("android") || platform == "ios" {
        let _slot = jobs.flutter_build_slot(job, project).await?;
        let platform = if platform == "ios" { "ios" } else { "android" };
        crate::launcher_icons::generate(&jobs.projects, project, job, &sdk, platform).await?;
    }
    let name = info["name"].as_str().unwrap_or(device).to_owned();
    let capabilities = &info["capabilities"];
    let mut args = vec!["run", "--machine", "--no-pub", "-d", device]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if info["platform"] == "web-javascript" {
        args.push("--no-web-resources-cdn".into());
    }
    args.extend(backends::options(&jobs.projects.home, project)?);
    args.extend(flags);
    job.check()?;
    job.report(json!({"device":name,"id":device,"capabilities":capabilities}));
    job.log(format!("Starting Flutter on {name}."));
    let mut process = jobs
        .flutter
        .command(&sdk, args)?
        .spawn(Some(&project.root), true)?;
    let result = run_session(jobs, job, project, &mut process, name, capabilities).await;
    process.stop().await;
    result
}

async fn run_session(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    process: &mut Process,
    name: String,
    capabilities: &Value,
) -> Result<Value> {
    let mut state = Session {
        device: name,
        app: None,
        pending: None,
        counter: 0,
        error: None,
    };
    let mut last = preview::stamps(&project.root)?;
    let mut watch = preview::watch();
    let startup = Instant::now() + Duration::from_secs(420);
    loop {
        let deadline = state.pending.map_or(startup, |(_, deadline, _)| deadline);
        let idle = state.pending.is_none() && job.is_ready();
        let restart = tokio::select! {
            _ = job.cancel.cancelled() => break Ok(json!({})),
            status = process.child.wait() => {
                let status = status?;
                if status.success() && job.is_ready() { break Ok(json!({})); }
                break Err(Problem::new(422,state.error.take().unwrap_or_else(|| format!("Flutter exited with {status}. See the console."))));
            }
            line = process.lines.recv() => {
                let Some(line) = line else {
                    break Err(Problem::new(422,"The Flutter device session closed."));
                };
                if let Ok(events) = serde_json::from_str::<Vec<Value>>(&line) {
                    if events.into_iter().any(|message| state.event(job,message)) {
                        if let Some(error) = state.error.take() { break Err(Problem::new(422,error)); }
                        if !job.is_ready() { break Err(Problem::new(422,"Flutter stopped before the app started. See the console.")); }
                        break Ok(json!({}));
                    }
                } else {
                    device_log(job, &line);
                }
                continue;
            }
            _ = time::sleep_until(deadline), if !idle => {
                if state.pending.take().is_some() {
                    job.compile_error("Flutter timed out applying this change. Try Hot restart.".into());
                    continue;
                }
                break Err(Problem::new(504,"Flutter did not start on the device in seven minutes."));
            }
            _ = job.restart_app.notified(), if idle => true,
            _ = job.reload.notified(), if idle => false,
            _ = watch.tick(), if idle && capabilities["hotReload"] == true => {
                if jobs.busy(&project.id) { continue; }
                let next = preview::stamps(&project.root)?;
                let changed = next.code != last.code;
                if next.project != last.project {
                    job.log("Project configuration changed. Stop and run the app again to apply it.");
                }
                last = next;
                if !changed { continue; }
                false
            }
        };
        let supported = if restart { "hotRestart" } else { "hotReload" };
        if capabilities[supported] != true || state.app.is_none() {
            job.compile_error("This device does not support that action.".into());
            continue;
        }
        last = preview::stamps(&project.root)?;
        state.counter += 1;
        job.refreshing();
        let command = json!([{"id":state.counter,"method":"app.restart","params":{
            "appId":state.app,"fullRestart":restart,"pause":false}}]);
        if let Err(error) = process.write(&format!("{command}\n")).await {
            break Err(error);
        }
        state.pending = Some((
            state.counter,
            Instant::now() + Duration::from_secs(65),
            restart,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flutter_device_ids_accept_adb_endpoints_and_reject_options() {
        for device in [
            "emulator-5554",
            "127.0.0.1:5555",
            "[::1]:5555",
            "iphone-1.2",
        ] {
            assert!(valid_id(device));
        }
        for device in [
            "",
            "--device",
            "name with spaces",
            "host/path",
            "name\nanother",
        ] {
            assert!(!valid_id(device));
        }
        assert!(debug_url(&json!("http://127.0.0.1:8900/?uri=local")).is_some());
        assert!(debug_url(&json!("https://example.com/")).is_none());
    }
}
