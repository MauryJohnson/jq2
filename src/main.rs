use jaq_core::{
    data,
    load::{Arena, File, Loader},
    unwrap_valr,
    Ctx,
    Vars,
};
use jaq_json::{read, Val};

use std::{
    env,
    fs,
    io::{self, BufWriter, Write},
    path::PathBuf,
    time::Instant,
};

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crossterm::{
    cursor,
    event::{
        self,
        Event,
        KeyCode,
        KeyEventKind,
        KeyModifiers,
    },
    execute,
    queue,
    style::{
        Color,
        Print,
        ResetColor,
        SetForegroundColor,
    },
    terminal::{
        self,
        ClearType,
    },
};

use std::collections::VecDeque;


fn write_indent_string(
    output: &mut String,
    indent: usize,
) {
    output.push_str(&" ".repeat(indent));
}

fn ansi_color(color: Color) -> &'static str {
    match color {
        Color::Blue => "\x1b[34m",
        Color::Green => "\x1b[32m",
        Color::Yellow => "\x1b[33m",
        Color::Magenta => "\x1b[35m",
        Color::Cyan => "\x1b[36m",
        Color::DarkGrey => "\x1b[90m",
        _ => "\x1b[0m",
    }
}

const ANSI_RESET: &str = "\x1b[0m";

fn pretty_json_highlighted(
    value: &Val,
) -> String {
    let mut output = String::new();

    pretty_json_highlighted_inner(
        value,
        0,
        &mut output,
    );

    output.push('\n');

    output
}

fn pretty_json_highlighted_inner(
    value: &Val,
    indent: usize,
    output: &mut String,
) {
    match value {
        Val::Arr(items) => {
            if items.is_empty() {
                output.push_str("[]");
                return;
            }

            output.push_str("[\n");

            for (index, item) in items.iter().enumerate() {
                write_indent_string(
                    output,
                    indent + 2,
                );

                pretty_json_highlighted_inner(
                    item,
                    indent + 2,
                    output,
                );

                if index + 1 < items.len() {
                    output.push(',');
                }

                output.push('\n');
            }

            write_indent_string(
                output,
                indent,
            );

            output.push(']');
        }

        Val::Obj(object) => {
            if object.is_empty() {
                output.push_str("{}");
                return;
            }

            output.push_str("{\n");

            let len = object.len();

            for (index, (key, item)) in
                object.iter().enumerate()
            {
                write_indent_string(
                    output,
                    indent + 2,
                );

                // Object key
                output.push_str(
                    ansi_color(Color::Cyan)
                );

                // IMPORTANT:
                // jaq's key Display implementation already
                // renders the key as a JSON string.
                output.push_str(
                    &key.to_string()
                );

                output.push_str(ANSI_RESET);
                output.push_str(": ");

                pretty_json_highlighted_inner(
                    item,
                    indent + 2,
                    output,
                );

                if index + 1 < len {
                    output.push(',');
                }

                output.push('\n');
            }

            write_indent_string(
                output,
                indent,
            );

            output.push('}');
        }

        Val::BStr(_) => {
            output.push_str(
                ansi_color(Color::Green)
            );

            output.push_str(
                &value.to_string()
            );

            output.push_str(ANSI_RESET);
        }

        Val::Bool(_) => {
            output.push_str(
                ansi_color(Color::Yellow)
            );

            output.push_str(
                &value.to_string()
            );

            output.push_str(ANSI_RESET);
        }

        Val::Null => {
            output.push_str(
                ansi_color(Color::DarkGrey)
            );

            output.push_str("null");

            output.push_str(ANSI_RESET);
        }

        // Numbers and any remaining scalar values.
        _ => {
            output.push_str(
                ansi_color(Color::Magenta)
            );

            output.push_str(
                &value.to_string()
            );

            output.push_str(ANSI_RESET);
        }
    }
}

const MAX_INTERACTIVE_ARRAY_ITEMS: usize = 1_000;
const MAX_INTERACTIVE_OBJECT_FIELDS: usize = 1_000;
fn guard_interactive_value(
    value: &Val,
) -> Result<(), String> {
    match value {
        Val::Arr(items) => {
            if items.len() > MAX_INTERACTIVE_ARRAY_ITEMS {
                return Err(format!(
                    "Interactive result blocked: query produced an array \
                     containing {} items.\n\
                     \n\
                     Large arrays cannot be displayed interactively.\n\
                     Stream the elements instead:\n\
                     \n\
                       .[] | ...\n\
                     \n\
                     instead of:\n\
                     \n\
                       [.[] | ...]\n\
                     \n\
                     Or explicitly limit the result:\n\
                     \n\
                       [limit(100; .[] | ...)]",
                    items.len(),
                ));
            }
        }

        Val::Obj(object) => {
            if object.len() > MAX_INTERACTIVE_OBJECT_FIELDS {
                return Err(format!(
                    "Interactive result blocked: query produced an object \
                     containing {} fields.",
                    object.len(),
                ));
            }
        }

        _ => {}
    }

    Ok(())
}
fn looks_like_top_level_array_constructor(
    query: &str,
) -> bool {
    let trimmed = query.trim_start();

    !trimmed.starts_with(":aggregate") && trimmed.starts_with('[')
}

// ============================================================
// Root JSON type
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootType {
    Array,
    Object,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectMode {
    Truncate,
    Append,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagerAction {
    Next,
    Previous,
    Quit,

}

#[derive(Debug)]
struct Page {
    lines: Vec<String>,

    // Number of NEW jaq results whose rendering began
    // while generating this page.
    result_count: u64,
}

#[derive(Debug, Default)]
struct RenderState {
    // Lines from a previously generated jaq result that
    // did not fit on the previous page.
    pending_lines: VecDeque<String>,
}
fn render_value_lines(
    value: &Val,
) -> VecDeque<String> {
    let rendered =
        pretty_json_highlighted(value);

    rendered
        .lines()
        .map(str::to_owned)
        .collect()
}
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self, String> {
        terminal::enable_raw_mode()
            .map_err(|e| {
                format!(
                    "Unable to enable raw terminal mode: {e}"
                )
            })?;

        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn terminal_page_size() -> usize {
    match terminal::size() {
        Ok((_width, height)) => {
            // Reserve lines for the pager status/footer.
            usize::from(height).saturating_sub(3).max(5)
        }

        Err(_) => 21,
    }
}

fn pager_input() -> Result<PagerAction, String> {
    loop {
        let event = event::read()
            .map_err(|e| {
                format!("Unable to read terminal input: {e}")
            })?;

        let Event::Key(key) = event else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Enter => {
                return Ok(PagerAction::Next);
            }

            KeyCode::Backspace => {
                return Ok(PagerAction::Previous);
            }

            KeyCode::Char('q')
            | KeyCode::Char('Q') => {
                return Ok(PagerAction::Quit);
            }

            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return Ok(PagerAction::Quit);
            }

            _ => {}
        }
    }
}

fn display_page(
    page: &Page,
    current_page: usize,
    cached_pages: usize,
    query_finished: bool,
) -> Result<(), String> {
    let mut stdout = io::stdout();

    execute!(
        stdout,
        terminal::Clear(ClearType::All),
        cursor::MoveTo(0, 0),
    )
    .map_err(|e| {
        format!("Unable to redraw terminal: {e}")
    })?;

    // Raw mode does not necessarily translate \n into \r\n.
    // Explicitly return to column 0 after every line.
    for line in &page.lines {
        write!(stdout, "{line}\r\n")
            .map_err(|e| {
                format!("Unable to write page: {e}")
            })?;
    }

    write!(stdout, "\r\n")
        .map_err(|e| {
            format!("Unable to write page spacing: {e}")
        })?;

    let status = if current_page == 0 {
        if query_finished
	    && current_page + 1 == cached_pages
	{
	    format!(
	        "[page {}/{} | Enter: close | Backspace: previous | q: quit]",
	        current_page + 1,
	        cached_pages,
	    )
	}
        else {
            format!(
	        "[page 1/1 | Enter: close | Backspace: previous | q: quit]"
	    )
        }
            
    } else if query_finished {
        format!(
            "[page {}/{} | Enter: next | Backspace: previous | q: quit]",
            current_page + 1,
            cached_pages,
        )
    } else {
        format!(
            "[page {} | Enter: next | Backspace: previous | q: stop]",
            current_page + 1,
        )
    };

    write!(stdout, "{status}")
        .map_err(|e| {
            format!("Unable to write pager status: {e}")
        })?;

    stdout
        .flush()
        .map_err(|e| {
            format!("Unable to flush terminal: {e}")
        })
}

fn generate_next_page<I, E>(
    results: &mut I,
    render_state: &mut RenderState,
    page_size: usize,
) -> Result<Option<Page>, String>
where
    I: Iterator<Item = Result<Val, E>>,
    E: std::fmt::Debug,
{
    let mut lines = Vec::with_capacity(page_size);
    let mut result_count = 0u64;

    while lines.len() < page_size {
        // ====================================================
        // FIRST:
        // Drain already-rendered lines.
        //
        // This is what allows one giant JSON value to span
        // several pages without advancing jaq.
        // ====================================================

        while lines.len() < page_size {
            let Some(line) =
                render_state.pending_lines.pop_front()
            else {
                break;
            };

            lines.push(line);
        }

        // Page became full using pending lines.
        if lines.len() >= page_size {
            break;
        }

        // ====================================================
        // SECOND:
        // There are no pending lines, so only NOW request
        // another result from jaq.
        // ====================================================

        let Some(result) = results.next() else {
            break;
        };

        let value =
            result.map_err(|e| {
                format!(
                    "Query execution error: {e:?}"
                )
            })?;

        // Protect the interactive pager from huge aggregate
        // values such as:
        //
        //     [.[] | ...]
        //
        // assuming you added this guard previously.
        guard_interactive_value(&value)?;

        // ====================================================
        // THIRD:
        // Render exactly this one jaq result.
        // ====================================================

        render_state.pending_lines =
            render_value_lines(&value);

        result_count += 1;

        // Don't drain here manually.
        //
        // Loop back around so the pending-lines logic at the
        // top remains the single place responsible for
        // transferring rendered lines into the page.
    }

    if lines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Page {
            lines,
            result_count,
        }))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Redirection {
    query: String,
    filename: String,
    mode: RedirectMode,
}

// ============================================================
// Main
// ============================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filename = env::args()
        .nth(1)
        .ok_or("Usage: jq2 <json-file>")?;

    let path = PathBuf::from(&filename);

    if !path.is_file() {
        return Err(
            format!("File does not exist: {}", path.display()).into()
        );
    }

    let metadata = fs::metadata(&path)?;

    println!();
    println!("============================================================");
    println!("Persistent jq-compatible Query Session");
    println!("============================================================");
    println!();

    println!("File: {}", path.display());

    println!(
        "Size: {:.2} GiB",
        metadata.len() as f64 / 1024_f64.powi(3)
    );

    println!();
    println!("Reading + parsing JSON ONCE...");

    let start = Instant::now();

    // --------------------------------------------------------
    // Read the original JSON file exactly once.
    //
    // After parsing succeeds, all queries operate against
    // `dataset`.
    // --------------------------------------------------------

    let bytes = fs::read(&path)?;

    let dataset = read::parse_single(&bytes)
        .map_err(|e| format!("JSON parse error: {e:?}"))?;

    // bytes is no longer needed after parsing.
    drop(bytes);

    let load_time = start.elapsed();

    // --------------------------------------------------------
    // Determine root type once.
    //
    // We intentionally do NOT serialize the entire dataset
    // merely to determine its type.
    // --------------------------------------------------------

    let root_type = detect_root_type(&dataset)?;

    println!(
        "Loaded in {:.3} seconds.",
        load_time.as_secs_f64()
    );

    // --------------------------------------------------------
    // Determine root length.
    // --------------------------------------------------------

    match execute_single(&dataset, "length") {
        Ok(Some(value)) => {
            println!("Records: {value}");
        }

        Ok(None) => {
            println!("Records: unknown");
        }

        Err(error) => {
            eprintln!("Unable to determine record count: {error}");
        }
    }

    println!();

    println!(
        "Root type: {}",
        root_type_name(root_type)
    );

    println!();
    println!("Parsed JSON is resident in memory.");
    println!("The source file will NOT be reopened for queries.");
    println!();
    println!("Query output is streamed into `less`.");
    println!("Large results are NOT collected into a result Vec.");
    println!();
    println!("Type :help for commands.");
    println!();

    repl(dataset, root_type)?;

    println!();
    println!("Releasing dataset from memory.");
    println!("Session ended.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================
    // Helpers
    // ========================================================

    fn parse(json: &str) -> Val {
        read::parse_single(json.as_bytes())
            .expect("test JSON should parse")
    }

    fn query_one(
        dataset: &Val,
        query: &str,
    ) -> Val {
        execute_single(dataset, query)
            .expect("query should execute")
            .expect("query should produce a value")
    }

    fn query_string(
        dataset: &Val,
        query: &str,
    ) -> String {
        query_one(dataset, query).to_string()
    }

    fn megavul_sample() -> Val {
        parse(
            r#"
            [
                {
                    "cve_id": "CVE-2026-0001",
                    "is_vul": true,
                    "func_name": "bad_function",
                    "repo_name": "repo-a"
                },
                {
                    "cve_id": "CVE-2026-0002",
                    "is_vul": false,
                    "func_name": "safe_function",
                    "repo_name": "repo-b"
                },
                {
                    "cve_id": "CVE-2026-0003",
                    "is_vul": true,
                    "func_name": "another_bad_function",
                    "repo_name": "repo-a"
                }
            ]
            "#,
        )
    }

    // ========================================================
    // JSON parsing
    // ========================================================

    #[test]
    fn parses_array_dataset() {
        let dataset = parse(
            r#"[{"id":1},{"id":2}]"#
        );

        assert_eq!(
            query_string(&dataset, "length"),
            "2"
        );
    }

    #[test]
    fn parses_object_dataset() {
        let dataset = parse(
            r#"{"name":"MegaVul","count":353873}"#
        );

        assert_eq!(
            query_string(&dataset, ".name"),
            "\"MegaVul\""
        );
    }

    #[test]
fn pretty_prints_object() {
    let value =
        parse(
            r#"{"cve_id":"CVE-1","is_vul":true}"#
        );

    let mut output =
        Vec::new();

    write_pretty_json(
        &mut output,
        &value,
    )
    .unwrap();

    let output =
        String::from_utf8(output)
            .unwrap();

    assert!(output.contains(
        "\"cve_id\": \"CVE-1\""
    ));

    assert!(output.contains(
        "\"is_vul\": true"
    ));

    assert!(output.starts_with("{\n"));
    assert!(output.ends_with("}\n"));
}

#[test]
fn pretty_prints_nested_array() {
    let value =
        parse(
            r#"{"cwe_ids":["CWE-79","CWE-89"]}"#
        );

    let mut output =
        Vec::new();

    write_pretty_json(
        &mut output,
        &value,
    )
    .unwrap();

    let output =
        String::from_utf8(output)
            .unwrap();

    println!("{:?},{}",output,output.contains("\"cwe_ids\""));

    assert!(output.contains(
        r#""cwe_ids": ["#
    ));

    assert!(output.contains(
        "\"CWE-79\""
    ));

    assert!(output.contains(
        "\"CWE-89\""
    ));
}

#[test]
fn json_file_uses_pretty_output() {
    assert!(
        should_pretty_print_file(
            "results.json"
        )
    );
}

#[test]
fn jsonl_file_uses_compact_output() {
    assert!(
        !should_pretty_print_file(
            "results.jsonl"
        )
    );

    assert!(
        !should_pretty_print_file(
            "results.ndjson"
        )
    );
}

    // ========================================================
    // Root type detection
    // ========================================================

    #[test]
    fn detects_array_root() {
        let dataset = parse(
            r#"[1,2,3]"#
        );

        assert_eq!(
            detect_root_type(&dataset).unwrap(),
            RootType::Array
        );
    }

    #[test]
    fn detects_object_root() {
        let dataset = parse(
            r#"{"hello":"world"}"#
        );

        assert_eq!(
            detect_root_type(&dataset).unwrap(),
            RootType::Object
        );
    }

    #[test]
    fn detects_other_root() {
        let dataset = parse(
            r#""hello""#
        );

        assert_eq!(
            detect_root_type(&dataset).unwrap(),
            RootType::Other
        );
    }

    // ========================================================
    // Direct-root-field detection
    // ========================================================

    #[test]
    fn recognizes_direct_root_field() {
        assert_eq!(
            direct_root_field(".cve_id"),
            Some("cve_id".to_string())
        );
    }

    #[test]
    fn recognizes_is_vul_root_field() {
        assert_eq!(
            direct_root_field(".is_vul"),
            Some("is_vul".to_string())
        );
    }

    #[test]
    fn does_not_treat_array_index_as_root_field() {
        assert_eq!(
            direct_root_field(".[0].cve_id"),
            None
        );
    }

    #[test]
    fn does_not_treat_array_iterator_as_root_field() {
        assert_eq!(
            direct_root_field(".[].cve_id"),
            None
        );
    }

    #[test]
    fn does_not_treat_identity_as_root_field() {
        assert_eq!(
            direct_root_field("."),
            None
        );
    }

    #[test]
    fn does_not_treat_pipe_as_root_field() {
        assert_eq!(
            direct_root_field(". | length"),
            None
        );
    }

    // ========================================================
    // Validation
    // ========================================================

    #[test]
    fn rejects_direct_field_on_array_root() {
        let result =
            validate_query(
                RootType::Array,
                ".cve_id",
            );

        assert!(result.is_err());

        let error = result.unwrap_err();

        assert!(
            error.contains(
                "root is an array"
            )
        );

        assert!(
            error.contains(
                ".[].cve_id"
            )
        );

        assert!(
            error.contains(
                ".[0].cve_id"
            )
        );
    }

    #[test]
    fn allows_direct_field_on_object_root() {
        assert!(
            validate_query(
                RootType::Object,
                ".cve_id"
            )
            .is_ok()
        );
    }

    #[test]
    fn allows_array_index() {
        assert!(
            validate_query(
                RootType::Array,
                ".[0].cve_id"
            )
            .is_ok()
        );
    }

    #[test]
    fn allows_array_iterator() {
        assert!(
            validate_query(
                RootType::Array,
                ".[].cve_id"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_bad_jq_syntax() {
        let result =
            validate_query(
                RootType::Array,
                ".[] | select(.is_vul =="
            );

        assert!(result.is_err());
    }

    #[test]
    fn rejects_empty_query() {
        assert!(
            validate_query(
                RootType::Array,
                ""
            )
            .is_err()
        );
    }

    // ========================================================
    // Basic jq execution
    // ========================================================

    #[test]
    fn gets_first_record_cve() {
        let dataset =
            megavul_sample();

        assert_eq!(
            query_string(
                &dataset,
                ".[0].cve_id"
            ),
            "\"CVE-2026-0001\""
        );
    }

    #[test]
    fn gets_second_record_cve() {
        let dataset =
            megavul_sample();

        assert_eq!(
            query_string(
                &dataset,
                ".[1].cve_id"
            ),
            "\"CVE-2026-0002\""
        );
    }

    #[test]
    fn gets_dataset_length() {
        let dataset =
            megavul_sample();

        assert_eq!(
            query_string(
                &dataset,
                "length"
            ),
            "3"
        );
    }

    #[test]
    fn accesses_boolean_field() {
        let dataset =
            megavul_sample();

        assert_eq!(
            query_string(
                &dataset,
                ".[0].is_vul"
            ),
            "true"
        );
    }

    // ========================================================
    // MegaVul-style queries
    // ========================================================

    #[test]
    fn counts_vulnerable_records() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                r#"
                [
                    .[]
                    | select(.is_vul == true)
                ]
                | length
                "#,
            );

        assert_eq!(
            result,
            "2"
        );
    }

    #[test]
    fn counts_non_vulnerable_records() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                r#"
                [
                    .[]
                    | select(.is_vul == false)
                ]
                | length
                "#,
            );

        assert_eq!(
            result,
            "1"
        );
    }

    #[test]
    fn selects_vulnerable_function_name() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                r#"
                first(
                    .[]
                    | select(.is_vul == true)
                    | .func_name
                )
                "#,
            );

        assert_eq!(
            result,
            "\"bad_function\""
        );
    }

    #[test]
    fn supports_map() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                "map(.cve_id) | length"
            );

        assert_eq!(
            result,
            "3"
        );
    }

    #[test]
    fn supports_select() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                r#"
                map(
                    select(
                        .repo_name == "repo-a"
                    )
                )
                | length
                "#,
            );

        assert_eq!(
            result,
            "2"
        );
    }

    #[test]
    fn supports_group_by() {
        let dataset =
            megavul_sample();

        let result =
            query_string(
                &dataset,
                r#"
                group_by(.repo_name)
                | length
                "#,
            );

        assert_eq!(
            result,
            "2"
        );
    }

    // ========================================================
    // Important regression tests
    // ========================================================

    #[test]
    fn query_does_not_modify_dataset() {
        let dataset =
            megavul_sample();

        let before =
            query_string(
                &dataset,
                "length"
            );

        let _ =
            query_string(
                &dataset,
                ".[0].cve_id"
            );

        let after =
            query_string(
                &dataset,
                "length"
            );

        assert_eq!(
            before,
            after
        );

        assert_eq!(
            after,
            "3"
        );
    }

    #[test]
fn parses_truncate_redirection() {
    let result =
        parse_redirection(
            ".[] | .cve_id > cves.jsonl"
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        result.query,
        ".[] | .cve_id"
    );

    assert_eq!(
        result.filename,
        "cves.jsonl"
    );

    assert_eq!(
        result.mode,
        RedirectMode::Truncate
    );
}


#[test]
fn parses_append_redirection() {
    let result =
        parse_redirection(
            ".[] | .cve_id >> cves.jsonl"
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        result.query,
        ".[] | .cve_id"
    );

    assert_eq!(
        result.filename,
        "cves.jsonl"
    );

    assert_eq!(
        result.mode,
        RedirectMode::Append
    );
}


#[test]
fn jq_greater_than_is_not_redirection() {
    let result =
        parse_redirection(
            ".[] | select(.cvss_base_score > 7)"
        )
        .unwrap();

    assert_eq!(
        result,
        None
    );
}


#[test]
fn jq_comparison_and_redirection_can_coexist() {
    let result =
        parse_redirection(
            ".[] | select(.cvss_base_score > 7) > severe.jsonl"
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        result.query,
        ".[] | select(.cvss_base_score > 7)"
    );

    assert_eq!(
        result.filename,
        "severe.jsonl"
    );
}


#[test]
fn greater_than_inside_string_is_not_redirection() {
    let result =
        parse_redirection(
            r#".[] | select(.commit_msg == "x > y")"#
        )
        .unwrap();

    assert_eq!(
        result,
        None
    );
}


#[test]
fn save_results_to_file() {
    let dataset =
        megavul_sample();

    let path =
        std::env::temp_dir()
            .join(
                format!(
                    "jq2-save-test-{}.jsonl",
                    std::process::id()
                )
            );

    let filename =
        path
            .to_string_lossy()
            .to_string();

    let count =
        execute_to_file(
            &dataset,
            ".[] | .cve_id",
            &filename,
            RedirectMode::Truncate,
        )
        .expect(
            "query should save"
        );

    assert_eq!(
        count,
        3
    );

    let contents =
        std::fs::read_to_string(
            &path
        )
        .expect(
            "saved file should exist"
        );

    let lines: Vec<_> =
        contents
            .lines()
            .collect();

    assert_eq!(
        lines.len(),
        3
    );

    assert_eq!(
        lines[0],
        "\"CVE-2026-0001\""
    );

    assert_eq!(
        lines[1],
        "\"CVE-2026-0002\""
    );

    assert_eq!(
        lines[2],
        "\"CVE-2026-0003\""
    );

    let _ =
        std::fs::remove_file(
            path
        );
}

    #[test]
    fn multiple_queries_use_same_dataset_value() {
        let dataset =
            megavul_sample();

        assert_eq!(
            query_string(
                &dataset,
                ".[0].cve_id"
            ),
            "\"CVE-2026-0001\""
        );

        assert_eq!(
            query_string(
                &dataset,
                ".[1].cve_id"
            ),
            "\"CVE-2026-0002\""
        );

        assert_eq!(
            query_string(
                &dataset,
                "length"
            ),
            "3"
        );
    }

    #[test]
    fn malformed_filter_fails_without_damaging_dataset() {
        let dataset =
            megavul_sample();

        assert!(
            validate_query(
                RootType::Array,
                ".[] | select("
            )
            .is_err()
        );

        assert_eq!(
            query_string(
                &dataset,
                "length"
            ),
            "3"
        );
    }
}

fn parse_redirection(
    input: &str,
) -> Result<Option<Redirection>, String> {
    let bytes = input.as_bytes();

    let mut in_string = false;
    let mut escaped = false;

    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;

    let mut i = 0usize;

    while i < bytes.len() {
        let ch = bytes[i] as char;

        // ---------------------------------------------
        // Inside a jq string
        // ---------------------------------------------
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }

            i += 1;
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
            }

            '(' => {
                paren_depth += 1;
            }

            ')' => {
                paren_depth =
                    paren_depth.saturating_sub(1);
            }

            '[' => {
                bracket_depth += 1;
            }

            ']' => {
                bracket_depth =
                    bracket_depth.saturating_sub(1);
            }

            '{' => {
                brace_depth += 1;
            }

            '}' => {
                brace_depth =
                    brace_depth.saturating_sub(1);
            }

            '>' => {
                // Only interpret > as file redirection
                // at the top level.
                //
                // This remains jq:
                //
                // select(.score > 7)
                //
                // This becomes redirection:
                //
                // .[] | .cve_id > output.jsonl

                if paren_depth == 0
                    && bracket_depth == 0
                    && brace_depth == 0
                {
                    let append =
                        i + 1 < bytes.len()
                            && bytes[i + 1] == b'>';

                    let operator_len =
                        if append {
                            2
                        } else {
                            1
                        };

                    let query =
                        input[..i].trim();

                    let filename =
                        input[i + operator_len..].trim();

                    if query.is_empty() {
                        return Err(
                            "Missing jq query before redirection."
                                .to_string(),
                        );
                    }

                    if filename.is_empty() {
                        return Err(
                            "Missing filename after redirection."
                                .to_string(),
                        );
                    }

                    if filename.contains('>') {
                        return Err(
                            "Invalid redirection syntax."
                                .to_string(),
                        );
                    }

                    return Ok(Some(
                        Redirection {
                            query: query.to_string(),

                            filename:
                                filename.to_string(),

                            mode:
                                if append {
                                    RedirectMode::Append
                                } else {
                                    RedirectMode::Truncate
                                },
                        },
                    ));
                }
            }

            _ => {}
        }

        i += 1;
    }

    Ok(None)
}

fn execute_to_file(
    dataset: &Val,
    query: &str,
    filename: &str,
    mode: RedirectMode,
) -> Result<u64, String> {
    // --------------------------------------------------------
    // Register jq definitions
    // --------------------------------------------------------

    let defs =
        jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());

    // --------------------------------------------------------
    // Register native jq functions
    // --------------------------------------------------------

    let funs =
        jaq_core::funs::<data::JustLut<Val>>()
            .chain(
                jaq_std::funs::<data::JustLut<Val>>()
            )
            .chain(
                jaq_json::funs::<data::JustLut<Val>>()
            );

    // --------------------------------------------------------
    // Compile query
    // --------------------------------------------------------

    let loader =
        Loader::new(defs);

    let arena =
        Arena::default();

    let program =
        File {
            code: query,
            path: (),
        };

    let modules =
        loader
            .load(
                &arena,
                program,
            )
            .map_err(
                |errors| {
                    format!(
                        "Unable to parse query:\n{errors:?}"
                    )
                },
            )?;

    let filter =
        jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(
                |errors| {
                    format!(
                        "Unable to compile query:\n{errors:?}"
                    )
                },
            )?;

    // --------------------------------------------------------
    // Open output file
    // --------------------------------------------------------

    let file =
        match mode {
            RedirectMode::Truncate => {
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(filename)
            }

            RedirectMode::Append => {
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .append(true)
                    .open(filename)
            }
        }
        .map_err(
            |e| {
                format!(
                    "Cannot open output file '{filename}': {e}"
                )
            },
        )?;

    let file_query =
        match mode {
            RedirectMode::Truncate => {
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(format!("{}.query",filename))
            }

            RedirectMode::Append => {
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .append(true)
                    .open(format!("{}.query",filename))
            }
        }
        .map_err(
            |e| {
                format!(
                    "Cannot open output file '{filename}': {e}"
                )
            },
        )?;        
    let mut output =
        BufWriter::new(file);
    
    let mut output2 = BufWriter::new(file_query);
    
    writeln!(output2,"{query}");

    output2
        .flush()
        .map_err(
            |e| {
                format!(
                    "Unable to flush '{filename}': {e}"
                )
            },
        )?;

    // --------------------------------------------------------
    // Create execution context
    // --------------------------------------------------------

    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    // --------------------------------------------------------
    // Execute against ALREADY-PARSED dataset
    // --------------------------------------------------------

    let results =
        filter
            .id
            .run(
                (
                    ctx,
                    dataset.clone(),
                )
            )
            .map(unwrap_valr);

    let mut count =
        0_u64;


    

    // --------------------------------------------------------
    // Stream results directly to disk
    // --------------------------------------------------------

    for result in results {
        let value =
            result
                .map_err(
                    |e| {
                        format!(
                            "Query execution error: {e:?}"
                        )
                    },
                )?;

        if should_pretty_print_file(filename) {
	    write_pretty_json(
	        &mut output,
	        &value,
	    )
	    .map_err(|e| {
	        format!(
	            "Unable to write to '{filename}': {e}"
	        )
	    })?;
	} else {
	    writeln!(
	        output,
	        "{value}"
	    )
	    .map_err(|e| {
	        format!(
	            "Unable to write to '{filename}': {e}"
	        )
	    })?;
	}
        
        count += 1;
    }

    // --------------------------------------------------------
    // Flush buffered output
    // --------------------------------------------------------

    output
        .flush()
        .map_err(
            |e| {
                format!(
                    "Unable to flush '{filename}': {e}"
                )
            },
        )?;

    Ok(count)
}

fn handle_redirection(
    dataset: &Val,
    root_type: RootType,
    input: &str,
) -> Result<Option<(u64, String, RedirectMode, String)>, String> {
    let Some(redirection) =
        parse_redirection(input)?
    else {
        return Ok(None);
    };

    // Validate ONLY the jq portion.
    validate_query(
        root_type,
        &redirection.query,
    )?;

    let count =
        execute_to_file(
            dataset,
            &redirection.query,
            &redirection.filename,
            redirection.mode,
        )?;

    Ok(
        Some(
            (
                count,
                redirection.filename,
                redirection.mode,
                redirection.query,
            )
        )
    )
}

fn handle_save_command(
    dataset: &Val,
    last_query: &Option<String>,
    input: &str,
) -> Result<Option<u64>, String> {
    if input == ":save" {
        return Err(
            "Usage: :save <filename>"
                .to_string(),
        );
    }

    let Some(filename) =
        input.strip_prefix(":save ")
    else {
        return Ok(None);
    };

    let filename =
        filename.trim();

    if filename.is_empty() {
        return Err(
            "Usage: :save <filename>"
                .to_string(),
        );
    }

    let Some(query) =
        last_query.as_ref()
    else {
        return Err(
            "No successful query is available to save."
                .to_string(),
        );
    };

    let count =
        execute_to_file(
            dataset,
            query,
            filename,
            RedirectMode::Truncate,
        )?;

    Ok(Some(count))
}

fn write_pretty_json<W: Write>(
    writer: &mut W,
    value: &Val,
) -> io::Result<()> {
    write_pretty_json_inner(writer, value, 0)?;
    writeln!(writer)
}

fn write_pretty_json_inner<W: Write>(
    writer: &mut W,
    value: &Val,
    indent: usize,
) -> io::Result<()> {
    match value {
        Val::Arr(values) => {
            if values.is_empty() {
                write!(writer, "[]")?;
                return Ok(());
            }

            writeln!(writer, "[")?;

            for (index, item) in values.iter().enumerate() {
                write_indent(writer, indent + 2)?;

                write_pretty_json_inner(
                    writer,
                    item,
                    indent + 2,
                )?;

                if index + 1 < values.len() {
                    write!(writer, ",")?;
                }

                writeln!(writer)?;
            }

            write_indent(writer, indent)?;
            write!(writer, "]")?;
        }

	Val::Obj(object) => {
	    if object.is_empty() {
	        write!(writer, "{{}}")?;
	        return Ok(());
	    }
	
	    writeln!(writer, "{{")?;
	
	    let len = object.len();
	
	    for (index, (key, item)) in
	        object.iter().enumerate()
	    {
	        write_indent(
	            writer,
	            indent + 2,
	        )?;
	
	        // jaq_json object keys already display
	        // as correctly quoted JSON strings.
	        write!(
	            writer,
	            "{}: ",
	            key
	        )?;
	
	        write_pretty_json_inner(
	            writer,
	            item,
	            indent + 2,
	        )?;
	
	        if index + 1 < len {
	            write!(
	                writer,
	                ","
	            )?;
	        }
	
	        writeln!(
	            writer
	        )?;
	    }
	
	    write_indent(
	        writer,
	        indent,
	    )?;
	
	    write!(
	        writer,
	        "}}"
	    )?;
	} 

        _ => {
            write!(writer, "{value}")?;
        }
    }

    Ok(())
}

fn write_indent<W: Write>(
    writer: &mut W,
    count: usize,
) -> io::Result<()> {
    for _ in 0..count {
        writer.write_all(b" ")?;
    }

    Ok(())
}

fn should_pretty_print_file(
    filename: &str,
) -> bool {
    let lower =
        filename.to_ascii_lowercase();

    !(lower.ends_with(".jsonl")
        || lower.ends_with(".ndjson"))
}

// ============================================================
// REPL
// ============================================================

fn repl(
    dataset: Val,
    root_type: RootType,
) -> Result<(), String> {
    let mut rl =
        DefaultEditor::new()
            .map_err(|e| format!("Unable to initialize line editor: {e}"))?;

    let mut last_query: Option<String> = None;

    loop {
        let line = match rl.readline("jq> ") {
            Ok(line) => line,

            // Ctrl-C: cancel current line but keep REPL alive
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }

            // Ctrl-D: exit
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }

            Err(err) => {
                return Err(
                    format!("Input error: {err}")
                );
            }
        };

        let query = line.trim();

        if query.is_empty() {
            continue;
        }

        match query {
            ":quit" | ":exit" => break,

            ":help" => {
                print_help();
                continue;
            }

            ":info" => {
                print_info(&dataset, root_type);
                continue;
            }

            ":last" => {
                match &last_query {
                    Some(q) => println!("{q}"),
                    None => println!("No previous query."),
                }

                continue;
            }

            ":clear" => {
                print!("\x1B[2J\x1B[1;1H");
                let _ = io::stdout().flush();
                continue;
            }

            _ => {}
        }

       // ============================================================
        // :save command
        // ============================================================
        
        if query == ":save"
            || query.starts_with(":save ")
        {
            match handle_save_command(
                &dataset,
                &last_query,
                query,
            ) {
                Ok(Some(count)) => {
                    let filename =
                        query
                            .strip_prefix(":save")
                            .unwrap_or("")
                            .trim();
        
                    println!(
                        "Saved {count} result(s) to {filename}"
                    );
                }
        
                Ok(None) => {}
        
                Err(err) => {
                    eprintln!("{err}");
                }
            }
        
            continue;
        }
        
        
        // ============================================================
        // Shell-style > / >> redirection
        // ============================================================
        
        match handle_redirection(
            &dataset,
            root_type,
            query,
        ) {
            Ok(Some(
                (
                    count,
                    filename,
                    mode,
                    executed_query,
                )
            )) => {
                // Store complete entered command in readline history.
                if let Err(err) =
                    rl.add_history_entry(query)
                {
                    eprintln!(
                        "Warning: unable to add command to history: {err}"
                    );
                }
        
                match mode {
                    RedirectMode::Truncate => {
                        println!(
                            "Saved {count} result(s) to {filename}"
                        );
                    }
        
                    RedirectMode::Append => {
                        println!(
                            "Appended {count} result(s) to {filename}"
                        );
                    }
                }
        
                // :last should represent the actual jq query,
                // not its output destination.
                last_query =
                    Some(executed_query);
        
                continue;
            }
        
            Ok(None) => {
                // No redirection.
                // Fall through to normal jq execution.
            }
        
            Err(err) => {
                eprintln!("{err}");
                continue;
            }
        }
        
        
        // ============================================================
        // Normal interactive jq execution
        // ============================================================
        
        if let Err(err) =
            validate_query(
                root_type,
                query,
            )
        {
            eprintln!("{err}");
            continue;
        }
        
        if let Err(err) =
            rl.add_history_entry(query)
        {
            eprintln!(
                "Warning: unable to add query to history: {err}"
            );
        }
        
        match execute_paged(
            &dataset,
            query,
        ) {
            Ok(stats) => {
                last_query =
                    Some(query.to_owned());
        
                if stats.broken_pipe {
                    continue;
                }
            }
        
            Err(err) => {
                eprintln!("{err}");
            }
        } 
        
        

        if let Err(err) =
            validate_query(root_type, query)
        {
            eprintln!("{err}");
            continue;
        }

        // Add only valid queries to history.
        if let Err(err) = rl.add_history_entry(query) {
            eprintln!(
                "Warning: unable to add query to history: {err}"
            );
        }

        
    }

    Ok(())
}

// ============================================================
// Query validation
// ============================================================

fn validate_query(
    root_type: RootType,
    query: &str,
) -> Result<(), String> {
    let query = query.trim();

    if query.is_empty() {
        return Err(
            "Query cannot be empty.".to_string()
        );
    }

    // --------------------------------------------------------
    // MegaVul root is an array.
    //
    // `.cve_id` is technically valid jq syntax, but it is the
    // wrong operation for an array root.
    //
    // Catch simple direct root-field lookups and provide a
    // useful correction.
    // --------------------------------------------------------

    if root_type == RootType::Array {
        if let Some(field) =
            direct_root_field(query)
        {
            return Err(format!(
                "Invalid operation for this dataset.\n\
                 \n\
                 The JSON root is an array, but `{query}` \
                 accesses `{field}` as though the root were \
                 an object.\n\
                 \n\
                 To access `{field}` from every record, use:\n\
                 \n\
                   .[].{field}\n\
                 \n\
                 To access `{field}` from the first record, use:\n\
                 \n\
                   .[0].{field}"
            ));
        }
    }

    // --------------------------------------------------------
    // Compile the query.
    //
    // This catches actual syntax/compiler errors before we
    // start the pager or execute against MegaVul.
    // --------------------------------------------------------

    validate_compile(query)?;

    Ok(())
}

// ============================================================
// Detect simple `.field` against root
// ============================================================

fn direct_root_field(
    query: &str,
) -> Option<String> {
    let query = query.trim();

    if !query.starts_with('.') {
        return None;
    }

    let rest = &query[1..];

    // These are NOT direct object-field accesses.
    if rest.is_empty()
        || rest.starts_with('[')
        || rest.starts_with('|')
    {
        return None;
    }

    let mut chars = rest.char_indices();

    let mut end = 0;

    while let Some((index, ch)) =
        chars.next()
    {
        if ch.is_ascii_alphanumeric()
            || ch == '_'
        {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }

    if end == 0 {
        return None;
    }

    let field = &rest[..end];
    let remainder = rest[end..].trim();

    // Only reject the simple:
    //
    // .cve_id
    //
    // Don't try to implement a second jq parser here.
    if remainder.is_empty() {
        Some(field.to_string())
    } else {
        None
    }
}

// ============================================================
// Validate compilation
// ============================================================

fn validate_compile(
    query: &str,
) -> Result<(), String> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());

    let funs = jaq_core::funs::<data::JustLut<Val>>()
    .chain(jaq_std::funs::<data::JustLut<Val>>())
    .chain(jaq_json::funs::<data::JustLut<Val>>());
    
    let loader = Loader::new(defs);

    let arena = Arena::default();

    let program = File {
        code: query,
        path: (),
    };

    let modules = loader
        .load(&arena, program)
        .map_err(|errors| {
            format!(
                "Invalid jq syntax:\n{errors:?}"
            )
        })?;

    jaq_core::Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            format!(
                "Unable to compile query:\n{errors:?}"
            )
        })?;

    Ok(())
}

// ============================================================
// Paged query statistics
// ============================================================

struct QueryStats {
    outputs: u64,
    broken_pipe: bool,
}

// ============================================================
// Execute query and stream directly to less
// ============================================================

fn execute_paged(
    dataset: &Val,
    query: &str,
) -> Result<QueryStats, String> {
    /*
    if looks_like_top_level_array_constructor(query) {
    return Err(
        "Interactive aggregate-array query blocked.\n\
         \n\
         This query appears to construct an entire array before returning it.\n\
         That prevents the interactive pager from interrupting execution.\n\
         \n\
         Prefer streaming:\n\
         \n\
           .[] | select(...)\n\
         \n\
         instead of:\n\
         \n\
           [.[] | select(...)]\n\
         \n\
         If you intentionally need an array, limit it first:\n\
         \n\
           [limit(100; .[] | select(...))]"
            .to_string()
    );
}*/

    // ========================================================
    // Compile jq query
    // ========================================================

    let defs =
        jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs());

    let funs =
        jaq_core::funs::<data::JustLut<Val>>()
            .chain(
                jaq_std::funs::<data::JustLut<Val>>()
            )
            .chain(
                jaq_json::funs::<data::JustLut<Val>>()
            );

    let loader = Loader::new(defs);
    let arena = Arena::default();
    let query2 = query.to_string().replace(":aggregate","");

    let program = File {
        code: query2.as_str(),
        path: (),
    };

    let modules =
        loader
            .load(&arena, program)
            .map_err(|errors| {
                format!(
                    "Unable to parse query:\n{errors:?}"
                )
            })?;

    let filter =
        jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errors| {
                format!(
                    "Unable to compile query:\n{errors:?}"
                )
            })?;

    // ========================================================
    // Create jq execution context
    // ========================================================

    let ctx =
        Ctx::<data::JustLut<Val>>::new(
            &filter.lut,
            Vars::new([]),
        );
let mut render_state = RenderState {
    pending_lines: VecDeque::new(),
};

    // IMPORTANT:
    //
    // Do NOT collect this iterator.
    //
    // Advancing `results` advances the jq query.
    // When we stop calling next(), jq stops producing output.
let mut results = filter
    .id
    .run((ctx, dataset.clone()))
    .map(unwrap_valr);

let _raw_mode = RawModeGuard::new()?;

let page_size = terminal_page_size();

let mut pages: Vec<Page> = Vec::new();

let mut render_state =
    RenderState::default();

let mut current_page = 0usize;

let mut query_finished = false;
let mut cancelled = false;

// Generate only the first page.
let first_page = generate_next_page(
    &mut results,
    &mut render_state,
    page_size,
)?;

let Some(first_page) = first_page else {
    drop(results);
    drop(_raw_mode);

    println!("[no results]");

    return Ok(QueryStats {
        outputs: 0,
        broken_pipe: false,
    });
};

pages.push(first_page);

loop {
    display_page(
        &pages[current_page],
        current_page,
        pages.len(),
        query_finished,
    )?;

    match pager_input()? {
        PagerAction::Previous => {
            if current_page > 0 {
                current_page -= 1;
            }
        }

	 PagerAction::Next => {
	    // -----------------------------------------------
	    // Cached page exists.
	    //
	    // Do NOT touch jaq.
	    // -----------------------------------------------
	
	    if current_page + 1 < pages.len() {
	        current_page += 1;
	        continue;
	    }
	
	    // -----------------------------------------------
	    // We already exhausted jaq AND there are no
	    // pending rendered lines.
	    // -----------------------------------------------
            

	    if query_finished {
	        break;
	    }
	
	    // -----------------------------------------------
	    // We're at the frontier.
	    //
	    // generate_next_page() will:
	    //
	    // 1. consume pending rendered lines first
	    // 2. only advance jaq when pending is empty
	    // -----------------------------------------------
	
	    match generate_next_page(
	        &mut results,
	        &mut render_state,
	        page_size,
	    )? {
	        Some(page) => {
	            pages.push(page);
	            current_page += 1;
	        }
	
	        None => {
	            query_finished = true;
                    break;
	        }
	    }
	}

        PagerAction::Quit => {
            cancelled = !query_finished;
            break;
        }
    }
}

drop(results);
drop(_raw_mode);

let outputs: u64 = pages
    .iter()
    .map(|page| page.result_count)
    .sum();

println!();

if cancelled {
    println!(
        "[stream stopped — {outputs} result(s) generated across {} page(s)]",
        pages.len(),
    );
} else {
    println!(
        "[pager closed — {outputs} result(s) across {} page(s)]",
        pages.len(),
    );
}

Ok(QueryStats {
    outputs,
    broken_pipe: cancelled,
})
}

// ============================================================
// Execute query and return only first result
//
// Used internally for things such as:
//
//     length
//     type
//
// This does NOT reopen the JSON file.
// ============================================================

fn execute_single(
    dataset: &Val,
    query: &str,
) -> Result<Option<Val>, String> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());

    let funs = jaq_core::funs::<data::JustLut<Val>>()
    .chain(jaq_std::funs::<data::JustLut<Val>>())
    .chain(jaq_json::funs::<data::JustLut<Val>>());

    let loader = Loader::new(defs);

    let arena = Arena::default();

    let program = File {
        code: query,
        path: (),
    };

    let modules =
        loader
            .load(&arena, program)
            .map_err(|errors| {
                format!(
                    "Query parse error: {errors:?}"
                )
            })?;

    let filter =
        jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errors| {
                format!(
                    "Query compile error: {errors:?}"
                )
            })?;

    let ctx =
        Ctx::<data::JustLut<Val>>::new(
            &filter.lut,
            Vars::new([]),
        );

    let mut output =
        filter
            .id
            .run((
                ctx,
                dataset.clone(),
            ))
            .map(unwrap_valr);

    match output.next() {
        Some(result) => {
            result
                .map(Some)
                .map_err(|error| {
                    format!(
                        "Query runtime error: {error:?}"
                    )
                })
        }

        None => Ok(None),
    }
}

// ============================================================
// Detect root type
// ============================================================

fn detect_root_type(
    dataset: &Val,
) -> Result<RootType, String> {
    let value =
        execute_single(
            dataset,
            "type",
        )?
        .ok_or_else(|| {
            "Unable to determine JSON root type."
                .to_string()
        })?;

    let type_name = value.to_string();

    match type_name.trim_matches('"') {
        "array" => Ok(RootType::Array),
        "object" => Ok(RootType::Object),
        _ => Ok(RootType::Other),
    }
}

// ============================================================
// Root type display
// ============================================================

fn root_type_name(
    root_type: RootType,
) -> &'static str {
    match root_type {
        RootType::Array => "array",
        RootType::Object => "object",
        RootType::Other => "other",
    }
}

// ============================================================
// Info
// ============================================================

fn print_info(
    dataset: &Val,
    root_type: RootType,
) {
    println!();

    println!(
        "Root type : {}",
        root_type_name(root_type)
    );

    match execute_single(
        dataset,
        "length",
    ) {
        Ok(Some(value)) => {
            println!(
                "Root length: {value}"
            );
        }

        Ok(None) => {
            println!(
                "Root length: unknown"
            );
        }

        Err(error) => {
            println!(
                "Root length: error ({error})"
            );
        }
    }

    println!("Parse mode: once");

    println!(
        "Storage   : persistent jaq_json::Val"
    );

    println!(
        "Output    : streamed through less"
    );

    println!();
}

// ============================================================
// Help
// ============================================================

fn print_help() {
    println!(
        r#"
Commands
========

:help
    Show this help.

:info
    Show information about the resident dataset.

:last
    Show the previous successful query.

:clear
    Clear the terminal.

:quit
:exit
    End the session.


MegaVul examples
================

First record:

    .[0]

First CVE:

    .[0].cve_id

Keys from the first record:

    .[0] | keys

All CVE IDs:

    .[].cve_id

Vulnerable function names:

    .[]
    | select(.is_vul == true)
    | .func_name

Count vulnerable records:

    [.[] | select(.is_vul == true)]
    | length

More memory-efficient vulnerable count:

    map(select(.is_vul == true))
    | length

Group records by CVE:

    group_by(.cve_id)
    | map({{
        cve: .[0].cve_id,
        count: length
      }})


Root-array protection
=====================

MegaVul's root is an array.

This will be rejected:

    .cve_id

Use:

    .[].cve_id

for every record, or:

    .[0].cve_id

for one record.


Pager
=====

Query results are streamed into `less`.

Useful controls:

    Up / Down      Scroll
    Space          Next page
    b              Previous page
    g              Beginning
    G              End
    /text          Search
    n              Next match
    N              Previous match
    q              Quit pager

Pressing q also stops consuming additional query results.

The original JSON file is parsed once at startup.
Each entered query is independently parsed/compiled.
"#
    );
}


