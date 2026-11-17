//! Literal shell segmentation only. It never evaluates expansions or scripts.
#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    pub operator: bool,
}
#[derive(Default)]
struct Lexer {
    segments: Vec<Vec<Word>>,
    words: Vec<Word>,
    heredocs: Vec<(String, bool)>,
    value: String,
    operator: String,
    have: bool,
    quoted: bool,
    want: String,
}
impl Lexer {
    fn flush(&mut self) {
        if !self.operator.is_empty() {
            let op = self
                .operator
                .trim_start_matches(|c: char| c.is_ascii_digit());
            if ["<<", "<<-"].contains(&op) {
                self.want = op.into();
            } else if op.chars().any(|c| ";&|()".contains(c)) && ![">&", "<&"].contains(&op) {
                self.segment();
            } else {
                self.words.push(Word {
                    text: self.operator.clone(),
                    operator: true,
                });
            }
        } else if self.have {
            if !self.want.is_empty() {
                self.heredocs.push((self.value.clone(), self.want == "<<-"));
                self.want.clear();
            } else {
                self.words.push(Word {
                    text: self.value.clone(),
                    operator: false,
                });
            }
        }
        self.value.clear();
        self.operator.clear();
        self.have = false;
        self.quoted = false;
    }
    fn segment(&mut self) {
        if !self.words.is_empty() {
            self.segments.push(std::mem::take(&mut self.words));
        }
    }
}
pub fn segments(command: &str) -> Result<Vec<Vec<Word>>, &'static str> {
    let text: Vec<char> = command.chars().collect();
    let mut lex = Lexer::default();
    let mut index = 0;
    let mut quote = None;
    while index < text.len() {
        let ch = text[index];
        index += 1;
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                lex.value.push(ch);
            }
            continue;
        }
        if quote == Some('"') {
            if ch == '"' {
                quote = None;
            } else if ch == '\\' && text.get(index).is_some_and(|c| "\"\\$`".contains(*c)) {
                lex.value.push(text[index]);
                index += 1;
            } else {
                lex.value.push(ch);
            }
            continue;
        }
        if ch == '\n' {
            lex.flush();
            lex.segment();
            for (delimiter, tabs) in std::mem::take(&mut lex.heredocs) {
                while index < text.len() {
                    let end = text[index..]
                        .iter()
                        .position(|c| *c == '\n')
                        .map_or(text.len(), |p| p + index);
                    let line: String = text[index..end].iter().collect();
                    index = (end + 1).min(text.len());
                    if (if tabs {
                        line.trim_start_matches('\t')
                    } else {
                        &line
                    }) == delimiter
                    {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '#' && !lex.have {
            if !lex.operator.is_empty() {
                lex.flush();
            }
            while index < text.len() && text[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if lex.operator.ends_with("<<") && ch == '-' {
            lex.operator.push(ch);
            continue;
        }
        if ";&|()<>".contains(ch) {
            if lex.operator.is_empty() {
                if lex.have
                    && !lex.quoted
                    && lex.value.chars().all(|c| c.is_ascii_digit())
                    && "<>".contains(ch)
                {
                    lex.operator = std::mem::take(&mut lex.value);
                    lex.have = false;
                } else {
                    lex.flush();
                }
            }
            lex.operator.push(ch);
            continue;
        }
        if !lex.operator.is_empty() {
            lex.flush();
        }
        match ch {
            ' ' | '\t' | '\r' => lex.flush(),
            '\\' => {
                if text.get(index) == Some(&'\n') {
                    index += 1;
                    continue;
                }
                if let Some(ch) = text.get(index) {
                    lex.value.push(*ch);
                    index += 1;
                }
                lex.have = true;
                lex.quoted = true;
            }
            '\'' | '"' => {
                quote = Some(ch);
                lex.have = true;
                lex.quoted = true;
            }
            _ => {
                lex.value.push(ch);
                lex.have = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote");
    }
    lex.flush();
    lex.segment();
    Ok(lex.segments)
}
pub fn assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}
pub fn command_index(words: &[String]) -> usize {
    words
        .iter()
        .position(|w| {
            !["command", "exec", "time", "nohup", "sudo", "env", "doas"].contains(&w.as_str())
                && !w.starts_with('-')
                && !assignment(w)
        })
        .unwrap_or(words.len())
}
pub fn words(segment: &[Word], skip_redirects: bool) -> Vec<String> {
    let mut skip = false;
    segment
        .iter()
        .filter_map(|w| {
            if w.operator {
                skip = skip_redirects;
                None
            } else if skip {
                skip = false;
                None
            } else {
                Some(w.text.clone())
            }
        })
        .collect()
}
pub fn patch_paths(command: &str, deleted: bool) -> Vec<String> {
    let mut names = Vec::new();
    let mut update = false;
    for line in command.lines() {
        if line.starts_with("*** Add File: ")
            || line.starts_with("*** Update File: ")
            || deleted
                && (line.starts_with("*** Delete File: ") || line.starts_with("*** Move to: "))
        {
            names.push(line.split_once(": ").unwrap().1.into());
            update = line.starts_with("*** Update File: ");
        } else if let Some(path) = line.strip_prefix("*** Move to: ")
            && update
        {
            if let Some(last) = names.last_mut() {
                *last = path.into();
            }
            update = false;
        } else if line.starts_with("*** ") {
            update = false;
        }
    }
    names
}
