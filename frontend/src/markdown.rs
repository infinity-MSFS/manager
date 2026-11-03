//! The small markdown subset release notes use: headings, bullet lists,
//! paragraphs and `**bold**`. Rendered as views, never as raw HTML, so
//! notes cannot inject markup.

use infinity_ui::prelude::*;

#[derive(Debug, PartialEq)]
enum Block<'a> {
    Heading(&'a str),
    List(Vec<&'a str>),
    Paragraph(String),
}

fn parse(text: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block<'_>>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    };
    for line in text.lines().map(str::trim) {
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            flush(&mut paragraph, &mut blocks);
            match blocks.last_mut() {
                Some(Block::List(items)) => items.push(item),
                _ => blocks.push(Block::List(vec![item])),
            }
        } else if line.starts_with('#') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(line.trim_start_matches('#').trim()));
        } else if line.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else {
            paragraph.push(line);
        }
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

/// `**bold**` spans; an unmatched `**` stays literal.
fn inline(text: &str) -> AnyView {
    let parts: Vec<&str> = text.split("**").collect();
    let balanced = parts.len() % 2 == 1;
    parts
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            if balanced && i % 2 == 1 {
                view! { <strong class="font-medium text-white/90">{part.to_owned()}</strong> }
                    .into_any()
            } else if !balanced && i > 0 {
                format!("**{part}").into_any()
            } else {
                part.to_owned().into_any()
            }
        })
        .collect_view()
        .into_any()
}

#[component]
pub fn Markdown(#[prop(into)] text: String) -> impl IntoView {
    parse(&text)
        .into_iter()
        .map(|block| match block {
            Block::Heading(h) => {
                view! { <h4 class="mt-4 mb-2 text-sm font-medium text-white/85 first:mt-0">{inline(h)}</h4> }
                    .into_any()
            }
            Block::List(items) => view! {
                <ul class="mb-2 list-disc space-y-1 pl-5 text-sm font-light text-white/65">
                    {items.into_iter().map(|i| view! { <li>{inline(i)}</li> }).collect_view()}
                </ul>
            }
            .into_any(),
            Block::Paragraph(p) => {
                view! { <p class="mb-2 text-sm font-light text-white/65">{inline(&p)}</p> }.into_any()
            }
        })
        .collect_view()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_notes() {
        let notes = "## 2.0.14\n\n- FIXED: a\n- ADDED: b\n\nSome text\nwrapped.\n* c";
        assert_eq!(
            parse(notes),
            vec![
                Block::Heading("2.0.14"),
                Block::List(vec!["FIXED: a", "ADDED: b"]),
                Block::Paragraph("Some text wrapped.".into()),
                Block::List(vec!["c"]),
            ]
        );
    }
}
