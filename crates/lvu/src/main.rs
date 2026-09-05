use std::{env, process::ExitCode};

use lvu::{App, fixture::FixtureProvider, terminal};

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let argument = arguments.next();
    if argument.as_deref() == Some("--help") {
        println!(
            "lvu --demo\n\nThe current standalone shell only runs an explicitly labelled synthetic fixture.\nA reviewed capture RowProvider will replace it during integration."
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
    if argument.as_deref() != Some("--demo") || arguments.next().is_some() {
        eprintln!(
            "lvu: this standalone shell requires --demo; production capture is not wired yet"
        );
        return ExitCode::from(2);
    }

    let (mut provider, sources, views) = FixtureProvider::demo();
    let app = App::new(sources, views, true);
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
