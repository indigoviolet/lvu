use std::{env, process::ExitCode};

use lvu::{App, fixture::FixtureProvider, terminal, theme::ThemeId};

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let argument = arguments.next();
    if argument.as_deref() == Some("--help") {
        println!(
            "lvu --demo [json]\n\nThe current standalone shell only runs an explicitly labelled synthetic fixture.\n`--demo json` selects the JSON-shaped fixture used by the highlighting PTY suite.\nA reviewed capture RowProvider will replace it during integration."
        );
        return ExitCode::SUCCESS;
    }
    if argument.as_deref() == Some("--demo-panic-restoration-probe") {
        return match terminal::panic_restoration_probe() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("lvu panic restoration probe: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let fixture = arguments.next();
    if argument.as_deref() != Some("--demo")
        || arguments.next().is_some()
        || !matches!(fixture.as_deref(), None | Some("json"))
    {
        eprintln!(
            "lvu: this standalone shell requires --demo [json]; production capture is not wired yet"
        );
        return ExitCode::from(2);
    }

    let json = fixture.as_deref() == Some("json");
    let (mut provider, sources, views) = if json {
        FixtureProvider::json_demo()
    } else {
        FixtureProvider::demo()
    };
    let mut app = App::new(sources, views, true);
    if json {
        // The JSON fixture exercises truecolor key/value roles, which the
        // terminal-inherited default palette deliberately does not define.
        app.theme_id = ThemeId::LoveDark;
    }
    let mut dispatcher = provider.query_dispatcher();
    match terminal::run(app, &mut provider, &mut dispatcher, |provider| {
        provider.advance()
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lvu: {error}");
            ExitCode::FAILURE
        }
    }
}
