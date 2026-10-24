// PC health: a quick look around the machine after Aura tasks (throttled) and
// hourly, scored 0–100 and kept as JSON lines for the `system` graphs.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIN_GAP_SECS: u64 = 300;
const CPU_WINDOW: Duration = Duration::from_millis(300);
const MAX_SANE_TEMP: f32 = 120.0;

static LAST: Mutex<Option<Sample>> = Mutex::new(None);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sample {
    pub ts: u64,
    pub score: u8,
    pub cpu: f32,
    pub mem: f32,
    pub swap: f32,
    pub disk: f32,
    pub temp: f32,
    pub errors: u32,
    pub issues: Vec<String>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn log_path() -> Option<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("aura-pane/health.jsonl"))
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// (idle, total) jiffies from the aggregate `cpu` line of /proc/stat.
fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|n| n.parse().ok())
        .collect();
    if nums.len() < 5 {
        return None;
    }
    Some((nums[3] + nums[4], nums.iter().sum()))
}

fn cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    parse_cpu_line(stat.lines().next()?)
}

fn cpu_percent() -> f32 {
    let Some((i1, t1)) = cpu_times() else { return 0.0 };
    std::thread::sleep(CPU_WINDOW);
    let Some((i2, t2)) = cpu_times() else { return 0.0 };
    let total = t2.saturating_sub(t1) as f32;
    if total == 0.0 {
        return 0.0;
    }
    100.0 * (1.0 - i2.saturating_sub(i1) as f32 / total)
}

/// (ram used %, swap used %)
fn mem_percent() -> (f32, f32) {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = |key: &str| -> f32 {
        info.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0.0)
    };
    let pct = |used: f32, total: f32| if total > 0.0 { 100.0 * used / total } else { 0.0 };
    let (total, avail) = (kb("MemTotal:"), kb("MemAvailable:"));
    let (stotal, sfree) = (kb("SwapTotal:"), kb("SwapFree:"));
    (pct(total - avail, total), pct(stotal - sfree, stotal))
}

/// `df -P` rows → (mount, used %), header skipped. One row per device: btrfs
/// subvolumes (/, /home, /var/log…) share a device and would count it several times.
fn parse_df(out: &str) -> Vec<(String, f32)> {
    let mut seen = std::collections::HashSet::new();
    out.lines()
        .skip(1)
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            let pct = cols.get(4)?.trim_end_matches('%').parse().ok()?;
            let mount = cols.get(5)?.to_string();
            seen.insert(*cols.first()?).then_some((mount, pct))
        })
        .collect()
}

fn disks() -> Vec<(String, f32)> {
    let out = run(
        "df",
        &["-P", "-l", "-x", "tmpfs", "-x", "devtmpfs", "-x", "efivarfs", "-x", "vfat",
          "-x", "squashfs", "-x", "overlay"],
    );
    parse_df(&out)
}

fn max_temp() -> f32 {
    let Ok(dirs) = fs::read_dir("/sys/class/hwmon") else { return 0.0 };
    dirs.flatten()
        .filter_map(|d| fs::read_dir(d.path()).ok())
        .flat_map(|files| files.flatten())
        .filter(|f| {
            let n = f.file_name();
            let n = n.to_string_lossy();
            n.starts_with("temp") && n.ends_with("_input")
        })
        .filter_map(|f| fs::read_to_string(f.path()).ok()?.trim().parse::<f32>().ok())
        .map(|milli| milli / 1000.0)
        .filter(|t| *t > 0.0 && *t < MAX_SANE_TEMP)
        .fold(0.0, f32::max)
}

fn failed_units() -> Vec<String> {
    let mut units = Vec::new();
    for args in [
        &["--failed", "--no-legend", "--plain"][..],
        &["--user", "--failed", "--no-legend", "--plain"][..],
    ] {
        units.extend(
            run("systemctl", args)
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(String::from)),
        );
    }
    units
}

fn journal_errors() -> u32 {
    run("journalctl", &["-p", "3", "--since", "-1h", "-q", "--no-pager", "-o", "cat"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count() as u32
}

/// 100 minus penalties; each penalty also explains itself in `issues`.
pub fn score(s: &Sample, disks: &[(String, f32)], failed: &[String]) -> (u8, Vec<String>) {
    let mut penalty = 0.0f32;
    let mut issues = Vec::new();
    if s.cpu > 85.0 {
        penalty += (s.cpu - 85.0) / 2.0;
        issues.push(format!("CPU busy {:.0}%", s.cpu));
    }
    if s.mem > 85.0 {
        penalty += (s.mem - 85.0) * 2.0;
        issues.push(format!("RAM {:.0}% used", s.mem));
    }
    if s.swap > 50.0 {
        penalty += 8.0;
        issues.push(format!("swap {:.0}% used", s.swap));
    }
    for (mount, pct) in disks.iter().filter(|(_, p)| *p >= 90.0) {
        penalty += (pct - 89.0) * 3.0;
        issues.push(format!("disk {mount} {pct:.0}% full"));
    }
    if s.temp > 85.0 {
        penalty += (s.temp - 85.0) * 2.0;
        issues.push(format!("running hot {:.0}°C", s.temp));
    }
    if !failed.is_empty() {
        penalty += (10.0 * failed.len() as f32).min(30.0);
        issues.extend(failed.iter().map(|u| format!("failed: {u}")));
    }
    if s.errors > 0 {
        penalty += (s.errors as f32 / 5.0).min(15.0);
        if s.errors >= 5 {
            issues.push(format!("{} errors in the log (last hour)", s.errors));
        }
    }
    ((100.0 - penalty).clamp(0.0, 100.0).round() as u8, issues)
}

fn take_sample() -> Sample {
    let (mem, swap) = mem_percent();
    let disks = disks();
    let failed = failed_units();
    let base = Sample {
        ts: now(),
        cpu: cpu_percent(),
        mem,
        swap,
        disk: disks.iter().map(|d| d.1).fold(0.0, f32::max),
        temp: max_temp(),
        errors: journal_errors(),
        ..Default::default()
    };
    let (score, issues) = score(&base, &disks, &failed);
    Sample { score, issues, ..base }
}

fn append(s: &Sample) {
    let Some(path) = log_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let line = match serde_json::to_string(s) {
        Ok(l) => l,
        Err(e) => return eprintln!("health: serialize failed: {e}"),
    };
    let res = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = res {
        eprintln!("health: can't write {}: {e}", path.display());
    }
}

/// One look around the PC. Reuses the last result if it is under 5 min old,
/// unless `force`.
pub fn check(force: bool) -> Sample {
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_none() {
        *last = history(1).pop();
    }
    if let Some(prev) = last.as_ref() {
        if !force && now().saturating_sub(prev.ts) < MIN_GAP_SECS {
            return prev.clone();
        }
    }
    let s = take_sample();
    append(&s);
    *last = Some(s.clone());
    s
}

// shortcut: reads the whole log each call; ~2.6 MB/year at hourly samples, prune if it ever lags.
pub fn history(days: u64) -> Vec<Sample> {
    let since = now().saturating_sub(days * 86_400);
    let Some(text) = log_path().and_then(|p| fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Sample>(l).ok())
        .filter(|s| s.ts >= since)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_line_idle_and_total() {
        assert_eq!(parse_cpu_line("cpu  10 0 10 70 10 0 0 0 0 0"), Some((80, 100)));
        assert_eq!(parse_cpu_line("cpu 1 2"), None);
    }

    #[test]
    fn parses_df_rows() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/nvme0n1p6 100 93 7 93% /\n";
        assert_eq!(parse_df(out), vec![("/".to_string(), 93.0)]);
    }

    #[test]
    fn counts_each_device_once() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/p8 100 93 7 93% /\n\
                   /dev/p8 100 93 7 93% /home\n\
                   /dev/p7 100 93 7 93% /mnt/bigdata\n";
        assert_eq!(parse_df(out), vec![("/".into(), 93.0), ("/mnt/bigdata".into(), 93.0)]);
    }

    #[test]
    fn healthy_machine_scores_100() {
        let s = Sample { cpu: 10.0, mem: 40.0, temp: 50.0, ..Default::default() };
        assert_eq!(score(&s, &[("/".into(), 50.0)], &[]), (100, vec![]));
    }

    #[test]
    fn full_disk_and_failed_unit_lower_score_and_explain() {
        let s = Sample { cpu: 10.0, mem: 40.0, ..Default::default() };
        let (score, issues) = score(&s, &[("/".into(), 95.0)], &["foo.service".into()]);
        assert_eq!(score, 72);
        assert_eq!(issues, vec!["disk / 95% full", "failed: foo.service"]);
    }
}
