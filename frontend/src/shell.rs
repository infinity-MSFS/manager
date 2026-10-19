use infinity_ipc::commands as cmd;
use infinity_ipc::dto::SessionState;
use infinity_ui::prelude::*;
use infinity_ui::{AccountItem, AccountMenu, TopNav, TopNavItem};
use leptos_router::hooks::{use_location, use_navigate};

use crate::brand::InfinityMark;
use crate::editorial::EYEBROW;
use crate::ext::use_extensions;
use crate::ipc;
use crate::routes;
use crate::state::use_app;
use crate::window::{WindowAction, perform};

#[component]
pub(crate) fn TitleBar() -> impl IntoView {
    view! {
        <header
            data-tauri-drag-region
            class="relative z-50 grid h-9 shrink-0 grid-cols-[1fr_auto_1fr] items-center border-b border-white/10 bg-white/[0.04] px-1 backdrop-blur-xl"
        >
            <div data-tauri-drag-region></div>
            <div data-tauri-drag-region class="flex items-center gap-2.5">
                <InfinityMark class="pointer-events-none h-3.5 w-3.5" />
                <span class=cn!("pointer-events-none", EYEBROW)>"Infinity Manager"</span>
            </div>
            <div class="flex h-full justify-self-end">
                <ChromeButton label="Minimize" action=WindowAction::Minimize>
                    <Icon icon=icons::MINUS class="h-4 w-4" />
                </ChromeButton>
                <ChromeButton label="Maximize" action=WindowAction::ToggleMaximize>
                    <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2">
                        <rect x="4" y="4" width="16" height="16" rx="1" />
                    </svg>
                </ChromeButton>
                <ChromeButton label="Close" action=WindowAction::Close danger=true>
                    <Icon icon=icons::X class="h-4 w-4" />
                </ChromeButton>
            </div>
        </header>
    }
}

#[component]
fn ChromeButton(
    label: &'static str,
    action: WindowAction,
    #[prop(optional)] danger: bool,
    children: Children,
) -> impl IntoView {
    view! {
        <button
            type="button"
            aria-label=label
            title=label
            class=cn!(
                "flex h-full w-11 items-center justify-center text-white/60 transition-colors duration-150",
                if danger { "hover:bg-red-500/80 hover:text-white" } else { "hover:bg-white/10 hover:text-white" },
            )
            on:click=move |_| perform(action)
        >
            {children()}
        </button>
    }
}

#[component]
pub(crate) fn NavBar() -> impl IntoView {
    let location = use_location();
    let navigate = use_navigate();
    let active = Signal::derive(move || location.pathname.get());
    let app = use_app();
    let extensions = use_extensions();
    let items = Signal::derive(move || {
        let mut items = vec![TopNavItem::new("My Products", routes::LIBRARY)];
        app.session.with(|session| {
            items.extend(
                extensions
                    .pages
                    .iter()
                    .filter(|p| p.gate.allows(session))
                    .map(|p| TopNavItem::new(p.label, p.path())),
            );
        });
        items.push(TopNavItem::new("Settings", routes::SETTINGS));
        items
    });

    view! {
        <TopNav
            class="app-nav relative z-40 shrink-0"
            items=items
            active=active
            on_select=Callback::new(move |href: String| navigate(&href, Default::default()))
            brand=ViewFn::from(|| ())
            actions=ViewFn::from(|| view! { <AccountControls /> })
            breakpoint=0.0
        />
    }
}

#[component]
fn AccountControls() -> impl IntoView {
    let app = use_app();
    let navigate = use_navigate();

    let items = Signal::derive(|| {
        vec![
            AccountItem::new("settings", "Account Settings").icon(icons::SETTINGS),
            AccountItem::new("logout", "Sign Out")
                .icon(icons::LOG_OUT)
                .tone(Tone::Danger),
        ]
    });
    let go = navigate.clone();
    let on_select = Callback::new(move |id: String| match id.as_str() {
        "settings" => go(routes::SETTINGS, Default::default()),
        "logout" => leptos::task::spawn_local(async {
            if let Err(e) = ipc::invoke::<cmd::AuthLogout>(&()).await {
                ipc::warn(&format!("sign out: {}", e.message));
            }
        }),
        _ => {}
    });

    move || {
        if !app.capabilities.get().accounts {
            return ().into_any();
        }
        match app.session.get() {
            SessionState::SignedIn { profile } => view! {
                <AccountMenu
                    name=profile
                        .name
                        .clone()
                        .or_else(|| profile.email.clone())
                        .unwrap_or_else(|| "Signed in".to_owned())
                    email=profile.email.clone()
                    picture=profile.picture.clone()
                    items=items
                    on_select=on_select
                />
            }
            .into_any(),
            _ => {
                let navigate = navigate.clone();
                view! {
                    <Button
                        variant=Variant::Solid
                        icon=icons::LOG_IN
                        on_click=Callback::new(move |()| navigate(routes::LOGIN, Default::default()))
                    >
                        "Sign In"
                    </Button>
                }
                .into_any()
            }
        }
    }
}
