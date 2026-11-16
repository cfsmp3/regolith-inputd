//! Values the daemon doesn't control (layouts, XKB options, the cursor
//! theme, device identifiers) written into sway commands so that sway
//! receives each exactly as it is.
//!
//! sway reads an IPC command in three steps. It splits the command list at
//! `;` and `,` outside quotes; splits each command into arguments at
//! whitespace outside quotes, and drops the quote marks of an argument
//! that starts with one, keeping its backslashes; and replaces the
//! variables of the user's config in each argument: `$name`, unless the
//! `$` follows a single backslash, with `$$` standing for `$`. A backslash
//! escapes the character after it in every step. Splitting the command
//! list also lets it escape the quote marks, `;`, `,` and backslashes that
//! directly follow that character, up to the next other character.

use std::error::Error;
use std::fmt::{self, Display};

/// The characters splitting the command list stops at.
const SPECIAL: [char; 5] = ['\\', '"', '\'', ';', ','];

/// Why a value can't be given to sway intact.
#[derive(Debug, PartialEq, Eq)]
pub enum Unquotable {
    /// sway's IPC ends a command at a line feed, quoted or not.
    LineFeed,
    /// sway reads an IPC payload as a C string, which ends at a NUL.
    Nul,
    /// An odd number of final backslashes would escape what follows the
    /// value.
    TrailingBackslash,
}

impl Display for Unquotable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unquotable::LineFeed => "it contains a line feed",
            Unquotable::Nul => "it contains a NUL character",
            Unquotable::TrailingBackslash => "it ends in an odd number of backslashes",
        })
    }
}

impl Error for Unquotable {}

/// `value` as one argument of a sway command, which sway hands to the
/// command exactly as it is. `quote` is the quote mark the command puts
/// around the argument: a value without quote marks, backslashes or `$` is
/// written between two of them, as it always was.
///
/// Other values are written as quoted runs side by side, each run quoted
/// with a mark it doesn't hold, or holds escaped. A run ends only where
/// every step of sway sees it end, or where splitting the command list
/// sees escaped characters only, the runs coming back to the one it
/// stays in before it sees anything again. Each `$` sway would replace is
/// doubled.
///
/// Final backslashes are left after the closing quote mark, where they
/// escape each other in pairs. `last` says the argument ends the command,
/// so nothing follows for an odd one to escape. Anywhere else an odd one
/// would escape the space after the argument, so such a value is refused,
/// as is one with a line feed or a NUL.
pub fn sway_argument(value: &str, quote: char, last: bool) -> Result<String, Unquotable> {
    if value.contains('\n') {
        return Err(Unquotable::LineFeed);
    }
    if value.contains('\0') {
        return Err(Unquotable::Nul);
    }
    let body = value.trim_end_matches('\\');
    let tail = &value[body.len()..];
    if !last && tail.len() % 2 == 1 {
        return Err(Unquotable::TrailingBackslash);
    }
    // Each character of the body with the number of backslashes before it.
    let mut chars = Vec::new();
    let mut backslashes = 0;
    for c in body.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            chars.push((backslashes, c));
            backslashes = 0;
        }
    }
    let start = if body.contains(quote) && !body.contains(other(quote)) {
        other(quote)
    } else {
        quote
    };
    let mut arg = String::new();
    let mut run: Option<char> = None;
    // The run splitting the command list is in, and whether it is escaping.
    let mut list_run = start;
    let mut escaping = false;
    for (i, &(backslashes, c)) in chars.iter().enumerate() {
        let current = run.unwrap_or(start);
        let escaped = escaping ^ (backslashes % 2 == 1);
        let special = SPECIAL.contains(&c);
        let next = if !special || (escaping && !escaped) {
            // Splitting the command list sees this character: be in its run.
            list_run
        } else if !escaping && escaped {
            // c is escaped for every step. The run chosen here is the one
            // splitting the command list stays in until it sees the next
            // character, which mustn't be its quote mark.
            match first_seen(&chars[i + 1..]) {
                Some(q) if q == current => other(q),
                _ => current,
            }
        } else if c == current {
            other(c)
        } else {
            current
        };
        if run != Some(next) {
            arg.extend(run);
            arg.push(next);
            run = Some(next);
        }
        if !escaping {
            list_run = next;
        }
        arg.push_str(&"\\".repeat(backslashes));
        arg.push(c);
        // A `$` after a single backslash is the one sway leaves alone.
        if c == '$' && backslashes != 1 {
            arg.push('$');
        }
        escaping = special && escaped;
    }
    match run {
        Some(q) => arg.push(q),
        None => arg.extend([start, start]),
    }
    arg.push_str(tail);
    Ok(arg)
}

fn other(quote: char) -> char {
    if quote == '\'' {
        '"'
    } else {
        '\''
    }
}

/// The character splitting the command list next sees unescaped, after an
/// escaped one, if it is one of those it stops at.
fn first_seen(chars: &[(usize, char)]) -> Option<char> {
    let mut escaping = true;
    for &(backslashes, c) in chars {
        if !SPECIAL.contains(&c) {
            return None;
        }
        escaping ^= backslashes % 2 == 1;
        if !escaping {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{sway_argument, Unquotable};

    #[test]
    fn plain_values_keep_their_quotes() {
        assert_eq!(sway_argument("us,de", '\'', true).unwrap(), "'us,de'");
        assert_eq!(sway_argument("", '\'', true).unwrap(), "''");
        assert_eq!(
            sway_argument("AZaz09_-:+()./", '"', false).unwrap(),
            "\"AZaz09_-:+()./\""
        );
    }

    #[test]
    fn quote_marks_go_in_runs_of_the_other_mark() {
        assert_eq!(
            sway_argument("My \"odd\" theme", '"', false).unwrap(),
            "'My \"odd\" theme'"
        );
        assert_eq!(
            sway_argument("a'b\"c", '\'', true).unwrap(),
            "'a'\"'b\"'\"c'"
        );
    }

    #[test]
    fn dollars_are_doubled_unless_sway_skips_them() {
        assert_eq!(sway_argument("$x$", '\'', true).unwrap(), "'$$x$$'");
        assert_eq!(sway_argument("\\$x", '\'', true).unwrap(), "'\\$x'");
        assert_eq!(sway_argument("\\\\$x", '\'', true).unwrap(), "'\\\\$$x'");
    }

    #[test]
    fn escaped_stretches_come_back_to_their_run() {
        // After `\` splitting the command list escapes `;'"`, so it never
        // sees the runs change; they are back in `'` before it sees `a`.
        assert_eq!(
            sway_argument("\\;'\"a\"b,c", '\'', true).unwrap(),
            "'\\;'\"'\"'\"a\"b,c'"
        );
    }

    #[test]
    fn final_backslashes_go_after_the_quotes() {
        assert_eq!(sway_argument("a\\", '\'', true).unwrap(), "'a'\\");
        assert_eq!(sway_argument("\\\\", '\'', true).unwrap(), "''\\\\");
        // In pairs they can come before other arguments too.
        assert_eq!(sway_argument("a\\\\", '"', false).unwrap(), "\"a\"\\\\");
        assert_eq!(
            sway_argument("a\\", '"', false),
            Err(Unquotable::TrailingBackslash)
        );
        assert_eq!(
            sway_argument("a\\\\\\", '"', false),
            Err(Unquotable::TrailingBackslash)
        );
    }

    #[test]
    fn line_feeds_and_nuls_are_refused() {
        assert_eq!(sway_argument("a\nb", '\'', true), Err(Unquotable::LineFeed));
        assert_eq!(sway_argument("a\0b", '"', false), Err(Unquotable::Nul));
    }

    use super::sway::{reads, strings};

    /// Every value up to `len` characters long over `alphabet`.
    fn all_values(alphabet: &[char], len: usize) -> Vec<String> {
        let mut values = vec![String::new()];
        let mut layer = vec![String::new()];
        for _ in 0..len {
            layer = layer
                .iter()
                .flat_map(|v| alphabet.iter().map(move |c| format!("{v}{c}")))
                .collect();
            values.extend(layer.iter().cloned());
        }
        values
    }

    const ALPHABET: [char; 8] = ['a', '\'', '"', '\\', '$', ';', ',', ' '];

    #[test]
    fn every_short_value_reaches_sway_intact_as_the_last_argument() {
        for value in all_values(&ALPHABET, 5) {
            for quote in ['\'', '"'] {
                let arg = sway_argument(&value, quote, true).unwrap();
                let command = format!("input type:keyboard xkb_layout {arg}");
                assert_eq!(
                    reads(&command),
                    vec![strings(&["input", "type:keyboard", "xkb_layout", &value])],
                    "{value:?} as {command:?}"
                );
            }
        }
    }

    #[test]
    fn every_short_value_reaches_sway_intact_before_other_arguments() {
        let values = all_values(&ALPHABET, 4);
        for value in &values {
            for quote in ['\'', '"'] {
                let trailing = value.chars().rev().take_while(|&c| c == '\\').count();
                let Ok(arg) = sway_argument(value, quote, false) else {
                    assert!(trailing % 2 == 1, "{value:?} refused");
                    continue;
                };
                // A plain argument and an odd one after it.
                for (next, next_arg) in [
                    ("none", "none".to_string()),
                    ("a\\';\"$b", sway_argument("a\\';\"$b", '"', true).unwrap()),
                ] {
                    let command = format!("input {arg} map_to_output {next_arg}");
                    assert_eq!(
                        reads(&command),
                        vec![strings(&["input", value, "map_to_output", next])],
                        "{value:?} as {command:?}"
                    );
                }
            }
        }
    }
}

/// How sway 1.11 reads an IPC command list, to check the quoting against: a
/// port of `execute_command` (sway/commands.c), with `argsep`, `split_args`
/// and `strip_quotes` (common/stringop.c) and `do_var_replacement`
/// (sway/config.c). Line feeds and criteria aren't ported.
#[cfg(test)]
pub(crate) mod sway {
    /// Variables a user's config might define, longest name first as sway
    /// sorts them.
    const VARIABLES: [(&str, &str); 4] = [
        ("$wm.layout", "de"),
        ("$mod", "Mod4"),
        ("$a", "VAR"),
        ("$", "DOLLAR"),
    ];

    const WHITESPACE: [char; 6] = [' ', '\x0c', '\n', '\r', '\t', '\x0b'];

    pub(crate) fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The commands sway runs for `payload`, each as the arguments its
    /// handler gets.
    pub(crate) fn reads(payload: &str) -> Vec<Vec<String>> {
        assert!(!payload.contains('\n') && !payload.starts_with('['));
        let mut commands = Vec::new();
        let mut head = Some(payload);
        while let Some(rest) = head {
            let (command, next) = argsep(rest.trim_start());
            head = next;
            let command = command.trim_start();
            if command.is_empty() {
                continue;
            }
            let mut argv = split_args(command);
            let strips = !matches!(argv[0].as_str(), "exec" | "exec_always" | "mode");
            for arg in argv.iter_mut().skip(1) {
                if strips && arg.starts_with(['"', '\'']) {
                    *arg = strip_quotes(arg);
                }
                *arg = var_replacement(arg);
            }
            commands.push(argv);
        }
        commands
    }

    /// One command of a list, and the rest of the list.
    fn argsep(list: &str) -> (&str, Option<&str>) {
        let b = list.as_bytes();
        let (mut start, mut end) = (0, 0);
        let (mut in_string, mut in_char, mut escaped) = (false, false, false);
        while let Some(offset) = b[end..].iter().position(|c| b"\"'\\;,".contains(c)) {
            let i = end + offset;
            if escaped && i != end {
                escaped = false;
            }
            match b[i] {
                b'"' if !in_char && !escaped => in_string = !in_string,
                b'\'' if !in_string && !escaped => in_char = !in_char,
                b'\\' => escaped = !escaped,
                b';' | b',' if !in_string && !in_char && !escaped => {
                    if i > start {
                        return (&list[start..i], Some(&list[i + 1..]));
                    }
                    start = i + 1;
                }
                _ => {}
            }
            end = i + 1;
        }
        (&list[start..], None)
    }

    fn split_args(command: &str) -> Vec<String> {
        let b = command.as_bytes();
        let is_space = |c: u8| WHITESPACE.contains(&(c as char));
        let mut args = Vec::new();
        let (mut start, mut end) = (0, 0);
        let (mut in_token, mut in_string, mut in_char) = (false, false, false);
        let (mut in_brackets, mut escaped) = (false, false);
        while start < b.len() {
            if !in_token {
                while end < b.len() && is_space(b[end]) {
                    end += 1;
                }
                start = end;
                in_token = true;
            }
            let c = b.get(end).copied().unwrap_or(0);
            if c == b'"' && !in_char && !escaped {
                in_string = !in_string;
            } else if c == b'\'' && !in_string && !escaped {
                in_char = !in_char;
            } else if c == b'[' && !in_string && !in_char && !in_brackets && !escaped {
                in_brackets = true;
            } else if c == b']' && !in_string && !in_char && in_brackets && !escaped {
                in_brackets = false;
            } else if c == b'\\' {
                escaped = !escaped;
            } else if c == 0 || (!in_string && !in_char && !in_brackets && !escaped && is_space(c))
            {
                if end > start {
                    args.push(command[start..end].to_string());
                }
                in_token = false;
                escaped = false;
                continue;
            }
            if c != b'\\' {
                escaped = false;
            }
            end += 1;
        }
        args
    }

    fn strip_quotes(arg: &str) -> String {
        let (mut in_str, mut in_chr, mut escaped) = (false, false, false);
        let mut out = String::new();
        for c in arg.chars() {
            if c == '\'' && !in_str && !escaped {
                in_chr = !in_chr;
            } else if c == '"' && !in_chr && !escaped {
                in_str = !in_str;
            } else if c == '\\' {
                escaped = !escaped;
                out.push(c);
            } else {
                escaped = false;
                out.push(c);
            }
        }
        out
    }

    fn var_replacement(arg: &str) -> String {
        let mut s = arg.as_bytes().to_vec();
        let mut find = 0;
        while let Some(offset) = s[find..].iter().position(|&c| c == b'$') {
            find += offset;
            if find > 0 && s[find - 1] == b'\\' && (find == 1 || s[find - 2] != b'\\') {
                find += 1;
                continue;
            }
            if s.get(find + 1) == Some(&b'$') {
                s.remove(find);
                find += 1;
                continue;
            }
            match VARIABLES
                .iter()
                .find(|(name, _)| s[find..].starts_with(name.as_bytes()))
            {
                Some((name, value)) => {
                    s.splice(find..find + name.len(), value.bytes());
                    find += value.len();
                }
                None => find += 1,
            }
        }
        String::from_utf8(s).unwrap()
    }

    #[test]
    fn the_port_reads_what_sway_1_11_reads() {
        // Checked with headless sway 1.11 and `sway -d`.
        for (payload, argument) in [
            (r#"input "1:2:Pen" left_handed enabled"#, "1:2:Pen"),
            (r#"input "x\"y" left_handed enabled"#, r#"x\"y"#),
            (r#"input "a'b"'"c' left_handed enabled"#, r#"a'b"c"#),
            (
                r#"input '$a $$a \$a \\$a' left_handed enabled"#,
                r#"VAR $a \$a \\VAR"#,
            ),
            (r#"input 'a\$b' left_handed enabled"#, r#"a\$b"#),
            (r#"input 'a\\$$b' left_handed enabled"#, r#"a\\$b"#),
            (r#"input '$wm.layout' left_handed enabled"#, "de"),
        ] {
            assert_eq!(
                reads(payload),
                vec![strings(&["input", argument, "left_handed", "enabled"])],
                "{payload}"
            );
        }
        assert_eq!(
            reads(r#"input "1:2:Pad";exec_x;"" left_handed enabled"#),
            vec![
                strings(&["input", "1:2:Pad"]),
                strings(&["exec_x"]),
                strings(&["\"\"", "left_handed", "enabled"]),
            ]
        );
    }
}
