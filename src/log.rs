// A record of every command agents give deskcrew, so the person can see afterwards which agent did
// what, and when. Typed text is left out, only its length is kept: it may be a password.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// The log is cut back to its newer half once it grows past this.
const KEEP_BYTES: u64 = 2_000_000;
const SHOWN_DEFAULT: usize = 30;
const REPLY_CHARS: usize = 160;

pub fn path() -> PathBuf {
    crate::shot::home().join("log.txt")
}

/// Add one command and how it went.
pub fn record(who: &str, words: &[String], ok: bool, reply: &str) {
    let first = reply.lines().next().unwrap_or("").trim();
    let first: String = first.chars().take(REPLY_CHARS).collect();
    let line = format!("{} {} {} {}  ->  {}\n", now(), quoted(who), if ok { "ok" } else { "failed" }, hidden(words), first);
    let file = path();
    if let Some(folder) = file.parent() {
        let _ = fs::create_dir_all(folder);
    }
    if fs::metadata(&file).is_ok_and(|m| m.len() > KEEP_BYTES)
        && let Ok(text) = fs::read_to_string(&file)
    {
        let half = text.len() / 2;
        let cut = text[half..].find('\n').map(|at| half + at + 1).unwrap_or(half);
        let _ = fs::write(&file, &text[cut..]);
    }
    if let Ok(mut out) = OpenOptions::new().create(true).append(true).open(&file) {
        let _ = out.write_all(line.as_bytes());
    }
}

/// `deskcrew log [n]`: the last n commands, oldest first.
pub fn show(words: &[String]) -> String {
    let count = words.get(1).and_then(|n| n.parse().ok()).unwrap_or(SHOWN_DEFAULT);
    let Ok(text) = fs::read_to_string(path()) else { return format!("no commands logged yet ({})", path().display()) };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    let mut out = lines[start..].join("\n");
    out.push_str(&format!("\n({} of {} commands; the full log is {})", lines.len() - start, lines.len(), path().display()));
    out
}

fn quoted(word: &str) -> String {
    if word.is_empty() || word.contains(char::is_whitespace) || word.contains('"') {
        format!("\"{}\"", word.replace('"', "'"))
    } else {
        word.to_string()
    }
}

fn length(text: &str) -> String {
    format!("<{} characters>", text.chars().count())
}

/// The command as it was given, with typed text and file contents swapped for their length.
fn hidden(words: &[String]) -> String {
    let flags_with_values = ["-w", "--window", "--agent", "--with", "--hold", "--ms", "--expect", "--risk", "--region", "--edge", "--from", "--find"];
    // Positions of the words that are not flags or flag values.
    let mut plain = Vec::new();
    let mut skip = false;
    for (at, word) in words.iter().enumerate() {
        if skip {
            skip = false;
            continue;
        }
        if flags_with_values.contains(&word.as_str()) {
            skip = true;
            continue;
        }
        if !word.starts_with("--") && !(word.starts_with('-') && word.len() == 2) {
            plain.push(at);
        }
    }
    let command = plain.first().map(|&at| words[at].as_str()).unwrap_or("");
    let secret: Vec<usize> = match command {
        // The text is the last plain word: type [control] "text", set control "value".
        "type" | "set" if plain.len() >= 2 => vec![*plain.last().unwrap()],
        "clip" if plain.get(1).is_some_and(|&at| words[at] == "set") => plain.get(2).copied().into_iter().collect(),
        "file" if plain.get(1).is_some_and(|&at| words[at] == "write") => plain.get(3).copied().into_iter().collect(),
        _ => Vec::new(),
    };
    words
        .iter()
        .enumerate()
        .map(|(at, word)| {
            if secret.contains(&at) {
                length(word)
            } else if command == "do" && plain.get(1) == Some(&at) {
                quoted(&hidden_steps(word))
            } else {
                quoted(word)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The steps of a `do` script, with the text of each `set` and `type` step swapped for its length.
fn hidden_steps(script: &str) -> String {
    script
        .split(';')
        .map(|step| {
            let parts = split_quoted(step.trim());
            match parts.first().map(String::as_str) {
                Some("set" | "type") if parts.len() >= 2 => {
                    let mut kept: Vec<String> = parts[..parts.len() - 1].iter().map(|p| quoted(p)).collect();
                    kept.push(length(&parts[parts.len() - 1]));
                    kept.join(" ")
                }
                _ => step.trim().to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Words split on spaces, keeping 'single' or "double" quoted ones together, quotes removed.
fn split_quoted(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started || !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => current.push(c),
        }
    }
    if started || !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// The time now in UTC, as 2026-11-13 09:12:05Z.
fn now() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970 to a calendar date (Howard Hinnant's method).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn typed_text_is_left_out() {
        assert_eq!(hidden(&words(&["type", "e3", "hunter2", "-w", "Login"])), "type e3 <7 characters> -w Login");
        assert_eq!(hidden(&words(&["set", "Password", "hunter2"])), "set Password <7 characters>");
        assert_eq!(hidden(&words(&["clip", "set", "secret text"])), "clip set <11 characters>");
        assert_eq!(hidden(&words(&["click", "Save", "--real"])), "click Save --real");
    }

    #[test]
    fn do_scripts_keep_their_steps_but_not_their_text() {
        let script = "set Name 'Ada Lovelace'; check 'Gift wrap'; type e3 \"pw\"; click Submit";
        assert_eq!(hidden(&words(&["do", script, "-w", "Form"])), "do \"set Name <12 characters>; check 'Gift wrap'; type e3 <2 characters>; click Submit\" -w Form");
    }

    #[test]
    fn dates_come_out_right() {
        let line = now();
        assert_eq!(line.len(), 20);
        assert!(line.starts_with("20"));
    }
}
