use infinity_ipc::{IpcError, IpcErrorCode};
use infinity_ui::prelude::*;

pub const EYEBROW: &str = "font-mono text-[11px] tracking-[0.25em] text-white/50 uppercase";

#[component]
pub fn PageHead(
    eyebrow: &'static str,
    title: &'static str,
    #[prop(optional)] accent: Option<&'static str>,
    #[prop(optional)] lead: Option<&'static str>,
    #[prop(optional)] aside: Option<ViewFn>,
) -> impl IntoView {
    view! {
        <header class="mb-14 flex flex-col gap-8 md:flex-row md:items-end md:justify-between">
            <div class="max-w-3xl">
                <p class=cn!("mb-8", EYEBROW)>{eyebrow}</p>
                <h1 class="text-5xl leading-[1.02] font-extralight tracking-tight">
                    {title}
                    {accent
                        .map(|a| {
                            view! {
                                " "
                                <span class="font-semibold text-accent-300">{a}</span>
                            }
                        })}
                </h1>
                {lead
                    .map(|l| {
                        view! {
                            <p class="mt-6 max-w-xl border-l border-accent-400/40 pl-5 text-base leading-relaxed font-light text-white/65">
                                {l}
                            </p>
                        }
                    })}
            </div>
            {aside.map(|a| view! { <div class="shrink-0">{a.run()}</div> })}
        </header>
    }
}

#[component]
pub fn SectionLabel(num: &'static str, label: &'static str) -> impl IntoView {
    view! {
        <div class=cn!("mb-8 flex items-center gap-4", EYEBROW)>
            <span class="text-accent-400/80">{num}</span>
            <span>{label}</span>
            <span class="h-px flex-1 bg-white/10"></span>
        </div>
    }
}

#[component]
pub fn HoverRule() -> impl IntoView {
    view! {
        <span class="absolute -top-px left-0 h-px w-0 bg-accent-400 transition-all duration-300 group-hover:w-full"></span>
    }
}

#[component]
pub fn HeaderStat(label: &'static str, #[prop(into)] value: Signal<String>) -> impl IntoView {
    view! {
        <div>
            <dt class="mb-1 tracking-[0.25em] text-white/40 uppercase">{label}</dt>
            <dd class="text-sm whitespace-nowrap text-accent-300">{move || value.get()}</dd>
        </div>
    }
}

#[component]
pub fn Page(children: Children) -> impl IntoView {
    view! {
        <AnimatedPage class="relative z-10">
            <div class="mx-auto max-w-7xl px-8 pt-12 pb-28">{children()}</div>
        </AnimatedPage>
    }
}

#[component]
pub fn ErrorNote(error: IpcError) -> impl IntoView {
    let tone = if error.code == IpcErrorCode::Unsupported {
        "border-white/20"
    } else {
        "border-red-400/50"
    };
    view! {
        <div class=cn!("border-l py-1 pl-5", tone)>
            <p class=cn!("mb-1", EYEBROW, "text-white/40")>{error.code.to_string()}</p>
            <p data-selectable class="font-light text-white/70">
                {error.message}
            </p>
        </div>
    }
}
