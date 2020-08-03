//! Repl implementation using [`rustyline`].
//!
//! [`rustyline`]: https://docs.rs/rustyline

use commands::default_commands;
use dirs_next::data_dir;
use helper::ReplHelper;
use hir::{Declaration, Expr};
use rustyline::{error::ReadlineError, Cmd, CompletionType, Config, EditMode, Editor, KeyPress};
use saltwater::{
    data, hir, initialize_jit_module, ir, types, CompileError, Locatable, Parser,
    PreProcessorBuilder, PureAnalyzer, SyntaxError, Type, JIT,
};
use std::{collections::HashMap, path::PathBuf};

mod commands;
mod helper;

/// The prefix for commands inside the repl.
const PREFIX: char = ':';
const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROMPT: &str = ">> ";

pub struct Repl {
    editor: Editor<ReplHelper>,
    commands: HashMap<&'static str, fn(&mut Repl, &str)>,
}

impl Repl {
    pub fn new() -> Self {
        let config = Config::builder()
            .history_ignore_space(true)
            .history_ignore_dups(false)
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .max_history_size(1000)
            .tab_stop(4)
            .build();
        let mut editor = Editor::with_config(config);

        let commands = default_commands();
        let helper = ReplHelper::new(commands.keys().copied().collect());
        editor.set_helper(Some(helper));

        editor.bind_sequence(KeyPress::Up, Cmd::LineUpOrPreviousHistory(1));
        editor.bind_sequence(KeyPress::Down, Cmd::LineDownOrNextHistory(1));
        editor.bind_sequence(KeyPress::Tab, Cmd::Complete);

        Self { editor, commands }
    }

    pub fn run(&mut self) -> rustyline::Result<()> {
        self.load_history();

        println!("Saltwater {}", VERSION);
        println!(r#"Type "{}help" for more information."#, PREFIX);
        let result = loop {
            let line = self.editor.readline(PROMPT);
            match line {
                Ok(line) => self.process_line(line),
                // Ctrl + c will abort the current line.
                Err(ReadlineError::Interrupted) => continue,
                // Ctrl + d will exit the repl.
                Err(ReadlineError::Eof) => break Ok(()),
                Err(err) => break Err(err),
            }
        };
        self.save_history();

        result
    }

    fn save_history(&self) -> Option<()> {
        let path = Self::history_path()?;
        self.editor.save_history(&path).ok()
    }

    fn load_history(&mut self) -> Option<()> {
        let path = Self::history_path()?;
        self.editor.load_history(&path).ok()
    }

    fn history_path() -> Option<PathBuf> {
        let mut history = data_dir()?;
        history.push("saltwater_history");
        Some(history)
    }

    fn process_line(&mut self, line: String) {
        self.editor.add_history_entry(line.clone());

        let line = line.trim();
        if line.starts_with(PREFIX) {
            let name = line.split(' ').next().unwrap();

            match self.commands.get(&name[1..]) {
                Some(action) => action(self, &line[name.len()..]),
                None => println!("unknown command '{}'", name),
            }
        } else {
            match self.execute_code(line) {
                Ok(_) => {}
                Err(err) => {
                    // TODO: Proper error reporting
                    println!("error: {}", err.data);
                }
            }
        }
    }

    fn execute_code(&mut self, code: &str) -> Result<(), CompileError> {
        let module = initialize_jit_module();

        let expr = analyze_expr(code)?;
        let expr_ty = expr.ctype.clone();
        let decl = wrap_expr(expr);
        let module = ir::compile(module, vec![decl], false).0?;

        let mut jit = JIT::from(module);
        jit.finalize();
        let execute_fun = jit
            .get_compiled_function("execute")
            .expect("this is not good.");

        match expr_ty {
            Type::Long(signed) => {
                let result = unsafe {
                    let execute: unsafe extern "C" fn() -> u64 = std::mem::transmute(execute_fun);
                    execute()
                };
                match signed {
                    true => println!("=> {}", result as i64),
                    false => println!("=> {}", result),
                }
            }
            // TODO: Implement execution for more types
            ty => println!("error: expression returns unsupported type: {:?}", ty),
        };
        Ok(())
    }
}

/// Takes an expression and wraps it into a `execute` function that looks like the following:
///
/// ```
/// <type> execute() {
///     return <expr>;
/// }
/// ```
fn wrap_expr(expr: Expr) -> Locatable<Declaration> {
    let fun = hir::Variable {
        ctype: types::Type::Function(types::FunctionType {
            return_type: Box::new(expr.ctype.clone()),
            params: vec![],
            varargs: false,
        }),
        storage_class: data::StorageClass::Extern,
        qualifiers: Default::default(),
        id: saltwater::InternedStr::get_or_intern("execute"),
    };

    let span = expr.location;
    let return_stmt = span.with(hir::StmtType::Return(Some(expr)));
    let init = hir::Initializer::FunctionBody(vec![return_stmt]);
    let decl = hir::Declaration {
        // FIXME: Currently doesn't work. Just make insert() pub?
        symbol: fun.insert(),
        init: Some(init),
    };
    span.with(decl)
}

fn analyze_expr(code: &str) -> Result<Expr, Locatable<SyntaxError>> {
    let code = format!("{}\n", code).into_boxed_str();
    let cpp = PreProcessorBuilder::new(code).build();
    let mut parser = Parser::new(cpp, false);
    let expr = parser.expr()?;

    let mut analyzer = PureAnalyzer::new();
    let expr = analyzer.expr(expr);

    // FIXME: error_handler is private so this doesn't work right now.
    // Please review and propose a solution.
    // if let Some(err) = analyzer.error_handler.pop_front() {
    //     return Err(err);
    // }

    Ok(expr)
}
