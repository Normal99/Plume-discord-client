//! Measures what a big account costs: decodes a synthetic READY shaped like
//! a heavy user's (100 servers × 60 channels, roles, overwrites, 300 DMs,
//! read states, plus the noise we skip: emoji, stickers, presences) and
//! applies it to a Feather store. No network or login needed.
//!
//!     cargo run --release -p plume-gateway --example ready_bench
//!
//! Prints JSON size, decode time and resident memory, so changes can be
//! compared. CI runs it on every build.

use std::fmt::Write as _;
use std::time::Instant;

use plume_core::{Profile, Store};
use serde_json::value::RawValue;

const GUILDS: usize = 100;
const CHANNELS_PER_GUILD: usize = 60;
const ROLES_PER_GUILD: usize = 40;
const DMS: usize = 300;

fn main() {
    let rss_start = rss_kb();
    let json = heavy_ready();
    let rss_json = rss_kb();

    let raw = RawValue::from_string(json).expect("valid JSON");
    let start = Instant::now();
    let decoded = plume_gateway::dispatch::decode("READY", &raw, None).expect("READY decodes");
    let decode_time = start.elapsed();
    let bytes = raw.get().len();
    drop(raw);

    let start = Instant::now();
    let mut store = Store::new(Profile::FEATHER);
    store.apply(decoded.event);
    let apply_time = start.elapsed();
    let rss_loaded = rss_kb();
    trim_heap();
    let rss_trimmed = rss_kb();

    println!("READY size:        {:.1} MB", bytes as f64 / 1_048_576.0);
    println!("decode:            {:.1} ms", decode_time.as_secs_f64() * 1000.0);
    println!("apply to store:    {:.1} ms", apply_time.as_secs_f64() * 1000.0);
    println!(
        "servers/channels:  {} / {}",
        store.guilds().count(),
        store.channels().count()
    );
    if let (Some(a), Some(b), Some(c), Some(d)) = (rss_start, rss_json, rss_loaded, rss_trimmed) {
        println!("RSS at start:      {:.1} MB", a as f64 / 1024.0);
        println!("RSS with JSON:     {:.1} MB", b as f64 / 1024.0);
        println!("RSS after READY:   {:.1} MB (JSON freed)", c as f64 / 1024.0);
        println!("RSS after trim:    {:.1} MB", d as f64 / 1024.0);
    }
}

/// A READY the size of a heavy account's, with realistic field noise.
fn heavy_ready() -> String {
    let mut out = String::with_capacity(16 << 20);
    out.push_str(r#"{"v":9,"user":{"id":"1","username":"me","global_name":"Me","avatar":"abc","discriminator":"0"},"#);
    out.push_str(r#""session_id":"s","resume_gateway_url":"wss://r","guilds":["#);
    for g in 0..GUILDS {
        let gid = 1_000_000 + g * 10_000;
        if g > 0 {
            out.push(',');
        }
        write!(
            out,
            r#"{{"id":"{gid}","data_mode":"full","lazy":true,"member_count":12345,"properties":{{"name":"Server number {g}","owner_id":"9","icon":"0123456789abcdef","description":"A server for testing things","features":["COMMUNITY","NEWS","INVITE_SPLASH"],"preferred_locale":"en-US","afk_timeout":300}},"roles":["#
        )
        .unwrap();
        for r in 0..ROLES_PER_GUILD {
            let id = if r == 0 { gid } else { gid + 5000 + r };
            if r > 0 {
                out.push(',');
            }
            write!(out, r#"{{"id":"{id}","name":"Role {r}","permissions":"1071698660929","color":3447003,"hoist":false,"position":{r},"managed":false,"mentionable":false,"flags":0,"icon":null,"unicode_emoji":null}}"#).unwrap();
        }
        out.push_str(r#"],"channels":["#);
        for c in 0..CHANNELS_PER_GUILD {
            if c > 0 {
                out.push(',');
            }
            let cid = gid + 1 + c;
            let kind = match c % 10 {
                0 => 4,
                9 => 2,
                _ => 0,
            };
            let parent = gid + 1 + (c / 10) * 10;
            write!(out, r#"{{"id":"{cid}","type":{kind},"name":"channel-{c}","position":{c},"parent_id":"{parent}","topic":"Talk about topic number {c} here, please be nice","last_message_id":"{}","rate_limit_per_user":0,"nsfw":false,"flags":0,"permission_overwrites":[{{"id":"{gid}","type":0,"allow":"0","deny":"1024"}},{{"id":"{}","type":0,"allow":"1024","deny":"0"}}]}}"#, 1_300_000_000_000_000_000u64 + cid as u64, gid + 5001).unwrap();
        }
        out.push_str(r#"],"emojis":["#);
        for e in 0..50 {
            if e > 0 {
                out.push(',');
            }
            write!(out, r#"{{"id":"{}","name":"emoji_{e}","roles":[],"require_colons":true,"managed":false,"animated":false,"available":true}}"#, gid + 8000 + e).unwrap();
        }
        out.push_str(r#"],"stickers":[],"threads":[],"stage_instances":[],"guild_scheduled_events":[]}"#);
    }
    out.push_str(r#"],"private_channels":["#);
    for d in 0..DMS {
        if d > 0 {
            out.push(',');
        }
        write!(
            out,
            r#"{{"id":"{}","type":1,"last_message_id":"{}","recipient_ids":["{}"],"flags":0}}"#,
            50_000_000 + d,
            1_300_000_000_000_000_000u64 + d as u64,
            60_000_000 + d
        )
        .unwrap();
    }
    out.push_str(r#"],"users":["#);
    for u in 0..DMS {
        if u > 0 {
            out.push(',');
        }
        write!(out, r#"{{"id":"{}","username":"friend{u}","global_name":"Friend {u}","avatar":"0123456789abcdef0123456789abcdef","discriminator":"0","public_flags":0,"avatar_decoration_data":null}}"#, 60_000_000 + u).unwrap();
    }
    out.push_str(r#"],"merged_members":["#);
    for g in 0..GUILDS {
        if g > 0 {
            out.push(',');
        }
        write!(out, r#"[{{"user_id":"1","roles":["{}"],"joined_at":"2024-01-01T00:00:00.000000+00:00","nick":null,"flags":0}}]"#, 1_000_000 + g * 10_000 + 5001).unwrap();
    }
    out.push_str(r#"],"read_state":{"version":1,"partial":false,"entries":["#);
    for g in 0..GUILDS {
        for c in 0..CHANNELS_PER_GUILD {
            if g + c > 0 {
                out.push(',');
            }
            let cid = 1_000_000 + g * 10_000 + 1 + c;
            write!(out, r#"{{"id":"{cid}","last_message_id":"{}","mention_count":{},"last_pin_timestamp":"2024-01-01T00:00:00+00:00","flags":0}}"#, 1_300_000_000_000_000_000u64 + cid as u64 - (c % 3) as u64, c % 7 / 6).unwrap();
        }
    }
    out.push_str(r#"]},"user_guild_settings":{"version":1,"partial":false,"entries":[{"guild_id":"1000000","muted":true,"channel_overrides":[]}]},"#);
    // Noise Plume never parses: experiments, presences, relationships.
    out.push_str(r#""experiments":["#);
    for e in 0..2000 {
        if e > 0 {
            out.push(',');
        }
        write!(out, "[{e},0,1,-1,0,0,0,0]").unwrap();
    }
    out.push_str(r#"],"relationships":["#);
    for r in 0..500 {
        if r > 0 {
            out.push(',');
        }
        write!(
            out,
            r#"{{"id":"{}","type":1,"nickname":null,"since":"2024-01-01T00:00:00+00:00"}}"#,
            70_000_000 + r
        )
        .unwrap();
    }
    out.push_str("]}");
    out
}

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only returns free memory to the OS.
    unsafe {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        malloc_trim(0);
    }
}
