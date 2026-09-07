//! Relocatable discovery of the two optional runtime resources.
//!
//! The viewer itself is a single self-contained executable. Advanced Polars
//! expressions additionally need the pinned Python helper project, and 🧠
//! assistance needs the built TypeScript bridge. Both used to be addressed with
//! `CARGO_MANIFEST_DIR`, so an installed copy could only work on the machine
//! that built it. Resolution now walks a documented precedence:
//!
//! 1. an explicit environment override ([`ENV_PYTHON_HELPER`],
//!    [`ENV_BRIDGE`], or [`ENV_RESOURCE_ROOT`] for both). An override is a
//!    pinned answer: when it does not contain the expected resource, lvu
//!    reports that and never quietly runs something from elsewhere.
//! 2. `libexec/lvu/<resource>` relative to the real executable. The executable
//!    path is canonicalized first, because Homebrew and mise both install
//!    through symlinks and the payload sits next to the real file.
//! 3. the user data location, `$XDG_DATA_HOME/lvu/libexec/<resource>`, with
//!    `~/.local/share/lvu/libexec/<resource>` as the fallback.
//! 4. the development checkout this executable was built from, so working in
//!    the repository behaves exactly as it did before.
//!
//! A missing resource is never fatal and never a panic: capture, literal and
//! `/regex/` search, native filtering, bookmarks and export all keep working,
//! and the resolver hands back an actionable diagnostic naming the resource,
//! where it looked and how to install it.

use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
};

/// Overrides the directory holding both `python` and `bridge` payloads.
pub const ENV_RESOURCE_ROOT: &str = "LVU_RESOURCE_ROOT";
/// Overrides the Python expression helper project directory.
pub const ENV_PYTHON_HELPER: &str = "LVU_PYTHON_HELPER_DIR";
/// Overrides the built Paseo bridge directory.
pub const ENV_BRIDGE: &str = "LVU_BRIDGE_DIR";

/// Relative location of packaged resources under an install prefix.
const PACKAGED_SUBDIR: &str = "libexec/lvu";
/// Pinned Node major/minor used by the development checkout.
const DEVELOPMENT_NODE: &str = "node@26.8.1";
/// Interpreter series required by `python/pyproject.toml`. uv provisions it.
const HELPER_PYTHON: &str = "3.12";
/// Exported lockfile a packaged helper carries instead of being built.
const HELPER_REQUIREMENTS: &str = "requirements.txt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resource {
    PythonHelper,
    AgentBridge,
}

impl Resource {
    /// Directory name inside a resource root; identical in the checkout.
    pub fn directory(self) -> &'static str {
        match self {
            Resource::PythonHelper => "python",
            Resource::AgentBridge => "bridge",
        }
    }

    pub fn override_variable(self) -> &'static str {
        match self {
            Resource::PythonHelper => ENV_PYTHON_HELPER,
            Resource::AgentBridge => ENV_BRIDGE,
        }
    }

    /// Files that must exist for a directory to be accepted as this resource.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Resource::PythonHelper => &["pyproject.toml", "lvu_expr_helper/__init__.py"],
            Resource::AgentBridge => &["dist/cli.js"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Resource::PythonHelper => "Python expression helper",
            Resource::AgentBridge => "agent bridge",
        }
    }

    /// What the user loses while the resource is absent.
    fn degraded(self) -> &'static str {
        match self {
            Resource::PythonHelper => {
                "advanced Polars filter and enrichment expressions are unavailable"
            }
            Resource::AgentBridge => "🧠 assistance is unavailable",
        }
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Named environment variable; searching stops here either way.
    Override(&'static str),
    /// `libexec/lvu` beside the real executable.
    Executable,
    /// XDG user data location.
    UserData,
    /// The checkout this executable was built from.
    Development,
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Override(variable) => write!(formatter, "{variable}"),
            Origin::Executable => formatter.write_str("installed beside the executable"),
            Origin::UserData => formatter.write_str("user data directory"),
            Origin::Development => formatter.write_str("development checkout"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Located {
    pub resource: Resource,
    pub path: PathBuf,
    pub origin: Origin,
    /// Exported locked requirements shipped beside a packaged helper. Its
    /// presence selects the invocation that leaves the install prefix alone.
    requirements: Option<PathBuf>,
    /// Virtual environment for `uv` when a packaged helper has no exported
    /// requirements and must still keep its environment out of the prefix.
    environment: Option<PathBuf>,
}

impl Located {
    fn development(&self) -> bool {
        self.origin == Origin::Development
    }

    /// Full argv for a Python helper module, program first.
    ///
    /// The development checkout keeps the exact pinned `mise exec -- uv run
    /// --project ... --locked` invocation it always used, so working in the
    /// repository is unchanged.
    ///
    /// A packaged helper must not be built in place: `uv run --project` makes
    /// setuptools write `build/` and `.egg-info` into the directory the
    /// package manager owns. The archive therefore ships the lockfile exported
    /// as hashed requirements, and the helper runs as a plain package on
    /// `PYTHONPATH` against an environment uv owns. Without those exported
    /// requirements (a directory a user pointed at by hand, say) the project
    /// form is used with its environment redirected out of the prefix.
    pub fn python_command(&self, module: &str, arguments: &[String]) -> Vec<String> {
        let project = self.path.to_string_lossy().into_owned();
        let mut argv = Vec::new();
        if self.development() {
            argv.extend([
                "mise".to_owned(),
                "exec".to_owned(),
                "--".to_owned(),
                "uv".to_owned(),
                "run".to_owned(),
                "--project".to_owned(),
                project,
                "--locked".to_owned(),
            ]);
        } else if let Some(requirements) = &self.requirements {
            argv.extend([
                "env".to_owned(),
                format!("PYTHONPATH={project}"),
                // Bytecode caches would otherwise land in the package
                // manager's directory on the first advanced expression.
                "PYTHONDONTWRITEBYTECODE=1".to_owned(),
                "uv".to_owned(),
                "run".to_owned(),
                "--no-project".to_owned(),
                "--python".to_owned(),
                HELPER_PYTHON.to_owned(),
                "--with-requirements".to_owned(),
                requirements.to_string_lossy().into_owned(),
            ]);
        } else {
            argv.push("env".to_owned());
            if let Some(environment) = &self.environment {
                argv.push(format!(
                    "UV_PROJECT_ENVIRONMENT={}",
                    environment.to_string_lossy()
                ));
            }
            argv.extend([
                "uv".to_owned(),
                "run".to_owned(),
                "--project".to_owned(),
                project,
                "--locked".to_owned(),
            ]);
        }
        argv.extend(["python".to_owned(), "-m".to_owned(), module.to_owned()]);
        argv.extend(arguments.iter().cloned());
        argv
    }

    /// Program, arguments and working directory for the built bridge.
    pub fn bridge_command(&self) -> (PathBuf, Vec<String>, PathBuf) {
        let arguments = if self.development() {
            vec![
                "exec".to_owned(),
                DEVELOPMENT_NODE.to_owned(),
                "--".to_owned(),
                "node".to_owned(),
                "dist/cli.js".to_owned(),
            ]
        } else {
            vec!["dist/cli.js".to_owned()]
        };
        let program = if self.development() { "mise" } else { "node" };
        (PathBuf::from(program), arguments, self.path.clone())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Missing {
    pub resource: Resource,
    /// Directories actually examined, in precedence order.
    pub searched: Vec<PathBuf>,
    /// Set when an environment override pinned the answer.
    pub pinned: Option<&'static str>,
}

impl Missing {
    /// One actionable line: what is gone, what still works, how to fix it.
    pub fn diagnostic(&self) -> String {
        let resource = self.resource;
        let searched = if self.searched.is_empty() {
            "no candidate directory was available".to_owned()
        } else {
            format!(
                "searched {}",
                self.searched
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let remedy = match self.pinned {
            Some(variable) => format!(
                "{variable} pins this location, so no other location was tried; \
                 unset it or point it at a directory containing {}",
                resource.markers().join(" and ")
            ),
            None => format!(
                "install lvu through Homebrew or mise, or set {} to a directory \
                 containing {}",
                resource.override_variable(),
                resource.markers().join(" and ")
            ),
        };
        format!(
            "{resource} not found: {}. {} ({remedy}). Capture, literal and /regex/ \
             search, native filtering and export remain available.",
            searched,
            resource.degraded()
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    Found(Located),
    Absent(Missing),
}

impl Resolution {
    pub fn located(&self) -> Option<&Located> {
        match self {
            Resolution::Found(located) => Some(located),
            Resolution::Absent(_) => None,
        }
    }

    pub fn diagnostic(&self) -> Option<String> {
        match self {
            Resolution::Found(_) => None,
            Resolution::Absent(missing) => Some(missing.diagnostic()),
        }
    }
}

/// Everything resolution reads from the outside world, so tests can supply it.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    variables: BTreeMap<&'static str, OsString>,
    /// Executable path as reported by the OS, before symlink resolution.
    executable: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Checkout root recorded at build time; absent on other machines.
    development_root: Option<PathBuf>,
}

impl Environment {
    /// Reads the real process environment.
    pub fn current() -> Self {
        let mut variables = BTreeMap::new();
        for name in [
            ENV_RESOURCE_ROOT,
            ENV_PYTHON_HELPER,
            ENV_BRIDGE,
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
        ] {
            if let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) {
                variables.insert(name, value);
            }
        }
        let development_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .ok();
        Self {
            variables,
            executable: env::current_exe().ok(),
            home: env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            development_root,
        }
    }

    #[cfg(test)]
    fn with_variable(mut self, name: &'static str, value: impl AsRef<OsStr>) -> Self {
        self.variables.insert(name, value.as_ref().to_os_string());
        self
    }

    #[cfg(test)]
    fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    #[cfg(test)]
    fn with_home(mut self, path: impl Into<PathBuf>) -> Self {
        self.home = Some(path.into());
        self
    }

    #[cfg(test)]
    fn with_development_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.development_root = Some(path.into());
        self
    }

    fn variable(&self, name: &'static str) -> Option<&OsStr> {
        self.variables.get(name).map(OsString::as_os_str)
    }

    /// Absolute XDG-style base, ignoring empty and relative values as the rest
    /// of the application does.
    fn base(&self, variable: &'static str, fallback: &str) -> Option<PathBuf> {
        self.variable(variable)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| self.home.as_ref().map(|home| home.join(fallback)))
    }

    fn uv_environment(&self) -> Option<PathBuf> {
        self.base("XDG_CACHE_HOME", ".cache")
            .map(|base| base.join("lvu/expr-env"))
    }
}

fn accepts(resource: Resource, directory: &Path) -> bool {
    resource
        .markers()
        .iter()
        .all(|marker| directory.join(marker).exists())
}

/// Directories checked beside the executable, real path first.
fn executable_candidates(environment: &Environment, resource: Resource) -> Vec<PathBuf> {
    let Some(executable) = &environment.executable else {
        return Vec::new();
    };
    // Homebrew and mise expose lvu through symlinks; the payload lives beside
    // the file the link points at, so the real path is checked first. The
    // reported path is still checked, which covers a hand-made link placed in
    // a directory that carries its own resources.
    let mut directories = Vec::new();
    for path in [executable.canonicalize().ok(), Some(executable.clone())]
        .into_iter()
        .flatten()
    {
        if let Some(parent) = path.parent()
            && !directories.contains(&parent.to_path_buf())
        {
            directories.push(parent.to_path_buf());
        }
    }
    let mut candidates = Vec::new();
    for directory in directories {
        // `bin/lvu` inside a prefix, then a flat directory carrying libexec.
        if let Some(prefix) = directory.parent() {
            candidates.push(prefix.join(PACKAGED_SUBDIR).join(resource.directory()));
        }
        candidates.push(directory.join(PACKAGED_SUBDIR).join(resource.directory()));
    }
    candidates
}

/// Resolves one resource against a supplied environment.
pub fn resolve(environment: &Environment, resource: Resource) -> Resolution {
    let uv_environment = environment.uv_environment();
    let found = |path: PathBuf, origin: Origin| {
        let requirements = (resource == Resource::PythonHelper)
            .then(|| path.join(HELPER_REQUIREMENTS))
            .filter(|candidate| candidate.exists());
        Resolution::Found(Located {
            resource,
            path,
            origin,
            requirements,
            environment: uv_environment.clone(),
        })
    };

    // An override answers the question outright. Falling through to another
    // location would run a different resource than the operator named.
    for variable in [resource.override_variable(), ENV_RESOURCE_ROOT] {
        let Some(value) = environment.variable(variable) else {
            continue;
        };
        let directory = if variable == ENV_RESOURCE_ROOT {
            PathBuf::from(value).join(resource.directory())
        } else {
            PathBuf::from(value)
        };
        return if accepts(resource, &directory) {
            found(directory, Origin::Override(variable))
        } else {
            Resolution::Absent(Missing {
                resource,
                searched: vec![directory],
                pinned: Some(variable),
            })
        };
    }

    let mut searched = Vec::new();
    let mut tiers: Vec<(Origin, Vec<PathBuf>)> = vec![(
        Origin::Executable,
        executable_candidates(environment, resource),
    )];
    if let Some(base) = environment.base("XDG_DATA_HOME", ".local/share") {
        tiers.push((
            Origin::UserData,
            vec![base.join("lvu/libexec").join(resource.directory())],
        ));
    }
    if let Some(root) = &environment.development_root {
        tiers.push((Origin::Development, vec![root.join(resource.directory())]));
    }
    for (origin, candidates) in tiers {
        for candidate in candidates {
            if accepts(resource, &candidate) {
                return found(candidate, origin);
            }
            searched.push(candidate);
        }
    }
    Resolution::Absent(Missing {
        resource,
        searched,
        pinned: None,
    })
}

/// Resolves both resources from the real environment.
pub fn resolve_all() -> (Resolution, Resolution) {
    let environment = Environment::current();
    (
        resolve(&environment, Resource::PythonHelper),
        resolve(&environment, Resource::AgentBridge),
    )
}

/// Human-readable resolution report behind `lvu-app --resources`.
///
/// Packaging scripts use this to prove a staged tree resolves to itself rather
/// than to whichever checkout happened to build the binary.
pub fn report() -> String {
    let environment = Environment::current();
    let mut text = String::new();
    text.push_str("lvu runtime resources\n");
    text.push_str(&format!(
        "executable: {}\n",
        environment
            .executable
            .as_ref()
            .and_then(|path| path.canonicalize().ok())
            .or_else(|| environment.executable.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unknown".to_owned())
    ));
    for resource in [Resource::PythonHelper, Resource::AgentBridge] {
        text.push('\n');
        match resolve(&environment, resource) {
            Resolution::Found(located) => {
                text.push_str(&format!(
                    "{}: found\n  path: {}\n  origin: {}\n",
                    resource,
                    located.path.display(),
                    located.origin
                ));
                let command = match resource {
                    Resource::PythonHelper => located.python_command("lvu_expr_helper", &[]),
                    Resource::AgentBridge => {
                        let (program, arguments, cwd) = located.bridge_command();
                        text.push_str(&format!("  cwd: {}\n", cwd.display()));
                        let mut command = vec![program.to_string_lossy().into_owned()];
                        command.extend(arguments);
                        command
                    }
                };
                text.push_str(&format!("  command: {}\n", command.join(" ")));
            }
            Resolution::Absent(missing) => {
                text.push_str(&format!(
                    "{}: missing\n  {}\n",
                    resource,
                    missing.diagnostic()
                ));
            }
        }
    }
    text.push_str(
        "\nPrerequisites: uv (advanced expressions), node (🧠 bridge) and an\n\
         authenticated agent CLI. Absent resources degrade features; capture,\n\
         literal and /regex/ search and native filtering do not need them.\n",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn stage(root: &Path, resource: Resource) -> PathBuf {
        let directory = root.join(resource.directory());
        for marker in resource.markers() {
            let file = directory.join(marker);
            fs::create_dir_all(file.parent().expect("marker parent")).expect("marker directory");
            fs::write(&file, b"staged").expect("marker file");
        }
        directory
    }

    fn stage_prefix(prefix: &Path) -> PathBuf {
        let executable = prefix.join("bin/lvu");
        fs::create_dir_all(executable.parent().expect("bin")).expect("bin directory");
        fs::write(&executable, b"#!/bin/sh\n").expect("executable");
        let libexec = prefix.join(PACKAGED_SUBDIR);
        stage(&libexec, Resource::PythonHelper);
        stage(&libexec, Resource::AgentBridge);
        executable
    }

    #[test]
    fn missing_resources_degrade_with_an_actionable_diagnostic() {
        let root = tempfile::tempdir().expect("temporary directory");
        let environment = Environment::default()
            .with_executable(root.path().join("opt/lvu/bin/lvu"))
            .with_home(root.path().join("home"));
        for resource in [Resource::PythonHelper, Resource::AgentBridge] {
            let resolution = resolve(&environment, resource);
            assert!(resolution.located().is_none());
            let diagnostic = resolution.diagnostic().expect("diagnostic");
            assert!(diagnostic.contains(resource.label()));
            assert!(diagnostic.contains(resource.override_variable()));
            assert!(diagnostic.contains("remain available"));
            // The searched list must name real candidates, not a placeholder.
            assert!(diagnostic.contains("libexec/lvu"));
        }
    }

    #[test]
    fn installed_prefix_resolves_relative_to_the_executable() {
        let root = tempfile::tempdir().expect("temporary directory");
        let prefix = root.path().join("prefix");
        let executable = stage_prefix(&prefix);
        let environment = Environment::default()
            .with_executable(&executable)
            .with_home(root.path().join("home"))
            // A development checkout is present and must lose to the install.
            .with_development_root(root.path().join("checkout"));
        stage(&root.path().join("checkout"), Resource::PythonHelper);
        let resolution = resolve(&environment, Resource::PythonHelper);
        let located = resolution.located().expect("python helper");
        assert_eq!(located.origin, Origin::Executable);
        assert_eq!(located.path, prefix.join("libexec/lvu/python"));
    }

    #[test]
    fn symlinked_executable_resolves_beside_the_real_file() {
        let root = tempfile::tempdir().expect("temporary directory");
        let prefix = root.path().join("Cellar/lvu/0.1.0");
        let executable = stage_prefix(&prefix);
        // Both Homebrew and mise expose the command through a link in a
        // directory that carries no resources of its own.
        let link_directory = root.path().join("bin");
        fs::create_dir_all(&link_directory).expect("link directory");
        let link = link_directory.join("lvu");
        std::os::unix::fs::symlink(&executable, &link).expect("symlink");
        let environment = Environment::default()
            .with_executable(&link)
            .with_home(root.path().join("home"));
        for resource in [Resource::PythonHelper, Resource::AgentBridge] {
            let resolution = resolve(&environment, resource);
            let located = resolution.located().expect("resource beside real file");
            assert_eq!(located.origin, Origin::Executable);
            assert_eq!(
                located.path,
                prefix.join(PACKAGED_SUBDIR).join(resource.directory())
            );
        }
    }

    #[test]
    fn user_data_precedes_the_development_checkout() {
        let root = tempfile::tempdir().expect("temporary directory");
        let data = root.path().join("data");
        stage(&data.join("lvu/libexec"), Resource::AgentBridge);
        let checkout = root.path().join("checkout");
        stage(&checkout, Resource::AgentBridge);
        let environment = Environment::default()
            .with_executable(root.path().join("elsewhere/lvu"))
            .with_variable("XDG_DATA_HOME", &data)
            .with_development_root(&checkout);
        let located = resolve(&environment, Resource::AgentBridge)
            .located()
            .cloned()
            .expect("bridge");
        assert_eq!(located.origin, Origin::UserData);
        assert_eq!(located.path, data.join("lvu/libexec/bridge"));
    }

    #[test]
    fn development_checkout_keeps_its_pinned_invocation() {
        let root = tempfile::tempdir().expect("temporary directory");
        let checkout = root.path().join("checkout");
        stage(&checkout, Resource::PythonHelper);
        stage(&checkout, Resource::AgentBridge);
        let environment = Environment::default()
            .with_executable(root.path().join("checkout/target/debug/lvu-app"))
            .with_development_root(&checkout);
        let helper = resolve(&environment, Resource::PythonHelper)
            .located()
            .cloned()
            .expect("helper");
        assert_eq!(helper.origin, Origin::Development);
        assert_eq!(
            helper.python_command("lvu_expr_helper", &[]),
            vec![
                "mise",
                "exec",
                "--",
                "uv",
                "run",
                "--project",
                &checkout.join("python").to_string_lossy(),
                "--locked",
                "python",
                "-m",
                "lvu_expr_helper",
            ]
        );
        let bridge = resolve(&environment, Resource::AgentBridge)
            .located()
            .cloned()
            .expect("bridge");
        let (program, arguments, cwd) = bridge.bridge_command();
        assert_eq!(program, PathBuf::from("mise"));
        assert_eq!(
            arguments,
            vec!["exec", DEVELOPMENT_NODE, "--", "node", "dist/cli.js"]
        );
        assert_eq!(cwd, checkout.join("bridge"));
    }

    #[test]
    fn a_packaged_helper_never_builds_inside_the_install_prefix() {
        let root = tempfile::tempdir().expect("temporary directory");
        let prefix = root.path().join("prefix");
        let executable = stage_prefix(&prefix);
        let helper_directory = prefix.join("libexec/lvu/python");
        let requirements = helper_directory.join(HELPER_REQUIREMENTS);
        fs::write(&requirements, b"polars==1.44.1\n").expect("exported requirements");
        let environment = Environment::default().with_executable(&executable);
        let helper = resolve(&environment, Resource::PythonHelper)
            .located()
            .cloned()
            .expect("helper");
        let command = helper.python_command("lvu_expr_helper.inspection", &["manifest".into()]);
        assert_eq!(command[0], "env");
        assert_eq!(
            command[1],
            format!("PYTHONPATH={}", helper_directory.display())
        );
        assert_eq!(command[2], "PYTHONDONTWRITEBYTECODE=1");
        // `--project` would let setuptools write build output into a directory
        // the package manager owns.
        assert!(!command.contains(&"--project".to_owned()));
        assert!(command.contains(&"--no-project".to_owned()));
        assert!(command.contains(&requirements.to_string_lossy().into_owned()));
        assert!(!command.contains(&"mise".to_owned()));
        assert_eq!(command.last().expect("argument"), "manifest");
        let bridge = resolve(&environment, Resource::AgentBridge)
            .located()
            .cloned()
            .expect("bridge");
        let (program, arguments, cwd) = bridge.bridge_command();
        assert_eq!(program, PathBuf::from("node"));
        assert_eq!(arguments, vec!["dist/cli.js"]);
        assert_eq!(cwd, prefix.join("libexec/lvu/bridge"));
    }

    #[test]
    fn a_hand_pointed_helper_without_exported_requirements_redirects_its_environment() {
        let root = tempfile::tempdir().expect("temporary directory");
        let staged = root.path().join("staged");
        let helper_directory = stage(&staged, Resource::PythonHelper);
        let environment = Environment::default()
            .with_executable(root.path().join("elsewhere/lvu"))
            .with_variable(ENV_PYTHON_HELPER, &helper_directory)
            .with_variable("XDG_CACHE_HOME", root.path().join("cache"));
        let helper = resolve(&environment, Resource::PythonHelper)
            .located()
            .cloned()
            .expect("helper");
        let command = helper.python_command("lvu_expr_helper", &[]);
        assert_eq!(command[0], "env");
        assert_eq!(
            command[1],
            format!(
                "UV_PROJECT_ENVIRONMENT={}",
                root.path().join("cache/lvu/expr-env").display()
            )
        );
        assert!(command.contains(&"--locked".to_owned()));
        assert!(!command.contains(&"mise".to_owned()));
    }

    #[test]
    fn an_override_pins_the_answer_and_never_falls_through() {
        let root = tempfile::tempdir().expect("temporary directory");
        let prefix = root.path().join("prefix");
        let executable = stage_prefix(&prefix);
        let empty = root.path().join("empty");
        fs::create_dir_all(&empty).expect("empty directory");
        let environment = Environment::default()
            .with_executable(&executable)
            .with_variable(ENV_PYTHON_HELPER, &empty);
        let resolution = resolve(&environment, Resource::PythonHelper);
        let diagnostic = resolution.diagnostic().expect("pinned diagnostic");
        assert!(resolution.located().is_none());
        assert!(diagnostic.contains(ENV_PYTHON_HELPER));
        assert!(diagnostic.contains("no other location was tried"));
        assert!(diagnostic.contains(&empty.display().to_string()));
        // The unpinned resource still resolves normally.
        assert!(
            resolve(&environment, Resource::AgentBridge)
                .located()
                .is_some()
        );
    }

    #[test]
    fn a_resource_root_override_supplies_both_payloads() {
        let root = tempfile::tempdir().expect("temporary directory");
        let staged = root.path().join("staged");
        stage(&staged, Resource::PythonHelper);
        stage(&staged, Resource::AgentBridge);
        let environment = Environment::default()
            .with_executable(root.path().join("elsewhere/lvu"))
            .with_variable(ENV_RESOURCE_ROOT, &staged);
        for resource in [Resource::PythonHelper, Resource::AgentBridge] {
            let located = resolve(&environment, resource)
                .located()
                .cloned()
                .expect("resource root payload");
            assert_eq!(located.origin, Origin::Override(ENV_RESOURCE_ROOT));
            assert_eq!(located.path, staged.join(resource.directory()));
        }
    }

    #[test]
    fn a_directory_without_its_markers_is_not_accepted() {
        let root = tempfile::tempdir().expect("temporary directory");
        let prefix = root.path().join("prefix");
        let executable = stage_prefix(&prefix);
        // A half-installed bridge (sources but no build output) must not be run.
        fs::remove_file(prefix.join("libexec/lvu/bridge/dist/cli.js")).expect("remove build");
        let environment = Environment::default().with_executable(&executable);
        assert!(
            resolve(&environment, Resource::AgentBridge)
                .located()
                .is_none()
        );
        assert!(
            resolve(&environment, Resource::PythonHelper)
                .located()
                .is_some()
        );
    }
}
