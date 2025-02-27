use std::{borrow::BorrowMut, io::Write, time::Duration, vec};

use crossterm::{
    cursor::SetCursorStyle,
    event::{poll, read, Event, KeyCode, KeyEvent, KeyModifiers},
    style::{Color, ContentStyle, StyledContent},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use shrs_core::{Context, Runtime, Shell};
use shrs_lang::{Lexer, Token};
use shrs_utils::cursor_buffer::{CursorBuffer, Location};
use shrs_vi::{Action, Command, Motion, Parser};

use crate::{
    buffer_history::{BufferHistory, DefaultBufferHistory},
    completion::{Completer, Completion, CompletionCtx, DefaultCompleter},
    cursor::CursorStyle,
    highlight::{DefaultHighlighter, Highlighter},
    history::{DefaultHistory, History},
    menu::{DefaultMenu, Menu},
    painter::{Painter, StyledBuf},
    prompt::{DefaultPrompt, Prompt},
    vi::ViCursorBuffer,
    DefaultKeybinding, Keybinding, LineModeSwitchCtx,
};

/// Operating mode of readline
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LineMode {
    /// Vi insert mode
    Insert,
    /// Vi normal mode
    Normal,
}

/// Configuration for readline
#[derive(Builder)]
#[builder(pattern = "owned")]
#[builder(setter(prefix = "with"))]
pub struct Line {
    #[builder(default = "Box::new(DefaultMenu::new())")]
    #[builder(setter(custom))]
    menu: Box<dyn Menu<MenuItem = Completion, PreviewItem = String>>,

    #[builder(default = "Box::new(DefaultCompleter::default())")]
    #[builder(setter(custom))]
    completer: Box<dyn Completer>,

    #[builder(default = "Box::new(DefaultHistory::new())")]
    #[builder(setter(custom))]
    history: Box<dyn History<HistoryItem = String>>,

    #[builder(default = "Box::new(DefaultBufferHistory::new())")]
    #[builder(setter(custom))]
    buffer_history: Box<dyn BufferHistory>,

    #[builder(default = "Box::new(DefaultHighlighter::default())")]
    #[builder(setter(custom))]
    highlighter: Box<dyn Highlighter>,

    #[builder(default = "Box::new(DefaultKeybinding::new())")]
    #[builder(setter(custom))]
    keybinding: Box<dyn Keybinding>,

    /// Custom prompt
    #[builder(default = "Box::new(DefaultPrompt::new())")]
    #[builder(setter(custom))]
    prompt: Box<dyn Prompt>,

    // ignored fields
    #[builder(default = "Painter::new()")]
    #[builder(setter(skip))]
    painter: Painter,

    /// Currently pressed keys in normal mode
    #[builder(default = "String::new()")]
    #[builder(setter(skip))]
    normal_keys: String,
}

impl Default for Line {
    fn default() -> Self {
        LineBuilder::default().build().unwrap()
    }
}

/// State for where the prompt is in history browse mode
#[derive(PartialEq, Eq)]
pub enum HistoryInd {
    /// Brand new prompt
    Prompt,
    /// In history line
    Line(usize),
}

impl HistoryInd {
    /// Go up (less recent) in history, if in prompt mode, then enter history
    pub fn up(&self, limit: usize) -> HistoryInd {
        match self {
            HistoryInd::Prompt => {
                if limit == 0 {
                    HistoryInd::Prompt
                } else {
                    HistoryInd::Line(0)
                }
            },
            HistoryInd::Line(i) => HistoryInd::Line((i + 1).min(limit)),
        }
    }

    /// Go down (more recent) in history, if in most recent history line, enter prompt mode
    pub fn down(&self) -> HistoryInd {
        match self {
            HistoryInd::Prompt => HistoryInd::Prompt,
            HistoryInd::Line(i) => {
                if *i == 0 {
                    HistoryInd::Prompt
                } else {
                    HistoryInd::Line(i.saturating_sub(1))
                }
            },
        }
    }
}

pub struct LineCtx<'a> {
    cb: CursorBuffer,
    // TODO this is temp, find better way to store prefix of current word
    current_word: String,
    // TODO dumping history index here for now
    history_ind: HistoryInd,
    // line contents that were present before entering history mode
    saved_line: String,
    mode: LineMode,
    pub lines: String,

    pub sh: &'a Shell,
    pub ctx: &'a mut Context,
    pub rt: &'a mut Runtime,
}

impl<'a> LineCtx<'a> {
    pub fn new(sh: &'a Shell, ctx: &'a mut Context, rt: &'a mut Runtime) -> Self {
        LineCtx {
            cb: CursorBuffer::new(),
            current_word: String::new(),
            history_ind: HistoryInd::Prompt,
            saved_line: String::new(),
            mode: LineMode::Insert,
            lines: "".to_string(),
            sh,
            ctx,
            rt,
        }
    }
    pub fn mode(&self) -> LineMode {
        self.mode
    }
}

// impl Default for LineCtx {
//     fn default() -> Self {
//         LineCtx {
//             cb: CursorBuffer::new(),
//             current_word: String::new(),
//             history_ind: HistoryInd::Prompt,
//             saved_line: String::new(),
//             mode: LineMode::Insert,
//         }
//     }
// }

// TODO none of the builder stuff is being autogenerated rn :()
impl LineBuilder {
    pub fn with_menu(
        mut self,
        menu: impl Menu<MenuItem = Completion, PreviewItem = String> + 'static,
    ) -> Self {
        self.menu = Some(Box::new(menu));
        self
    }
    pub fn with_completer(mut self, completer: impl Completer + 'static) -> Self {
        self.completer = Some(Box::new(completer));
        self
    }
    pub fn with_history(mut self, history: impl History<HistoryItem = String> + 'static) -> Self {
        self.history = Some(Box::new(history));
        self
    }
    pub fn with_highlighter(mut self, highlighter: impl Highlighter + 'static) -> Self {
        self.highlighter = Some(Box::new(highlighter));
        self
    }
    pub fn with_keybinding(mut self, keybinding: impl Keybinding + 'static) -> Self {
        self.keybinding = Some(Box::new(keybinding));
        self
    }
    pub fn with_prompt(mut self, prompt: impl Prompt + 'static) -> Self {
        self.prompt = Some(Box::new(prompt));
        self
    }
}

impl Line {
    /// Start readline and read one line of user input
    pub fn read_line(&mut self, sh: &Shell, ctx: &mut Context, rt: &mut Runtime) -> String {
        let mut line_ctx = LineCtx::new(sh, ctx, rt);
        self.read_events(&mut line_ctx).unwrap()
    }

    fn read_events(&mut self, line_ctx: &mut LineCtx) -> anyhow::Result<String> {
        // ensure we are always cleaning up whenever we leave this scope
        struct CleanUp;
        impl Drop for CleanUp {
            fn drop(&mut self) {
                disable_raw_mode();
            }
        }
        let _cleanup = CleanUp;

        enable_raw_mode()?;

        self.painter.init().unwrap();

        self.painter.paint(
            line_ctx,
            &self.prompt,
            &self.menu,
            StyledBuf::empty(),
            line_ctx.cb.cursor(),
        )?;

        loop {
            if poll(Duration::from_millis(1000))? {
                let event = read()?;

                if let Event::Key(key_event) = event {
                    if self.keybinding.handle_key_event(key_event) {
                        break;
                    }
                }

                let should_break = self.handle_standard_keys(line_ctx, event.clone())?;
                if should_break {
                    break;
                }

                // handle menu events
                if self.menu.is_active() {
                    self.handle_menu_keys(line_ctx, event.clone())?;
                } else {
                    match line_ctx.mode {
                        LineMode::Insert => {
                            self.handle_insert_keys(line_ctx, event)?;
                        },
                        LineMode::Normal => {
                            self.handle_normal_keys(line_ctx, event)?;
                        },
                    }
                }

                let res = self.get_full_command(line_ctx);

                // syntax highlight
                let mut styled_buf = self.highlighter.highlight(&res, line_ctx.lines.len());

                // add currently selected completion to buf
                if self.menu.is_active() {
                    if let Some(selection) = self.menu.current_selection() {
                        let trimmed_selection = &selection.accept()[line_ctx.current_word.len()..];
                        styled_buf.push(
                            trimmed_selection,
                            ContentStyle {
                                foreground_color: Some(Color::Red),
                                ..Default::default()
                            },
                        );
                    }
                }

                self.painter.paint(
                    line_ctx,
                    &self.prompt,
                    &self.menu,
                    styled_buf,
                    line_ctx.cb.cursor(),
                )?;
            }
        }

        let res = self.get_full_command(line_ctx);
        if !res.is_empty() {
            self.history.add(res.clone());
        }
        Ok(res)
    }

    fn handle_menu_keys(&mut self, ctx: &mut LineCtx, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if let Some(accepted) = self.menu.accept().cloned() {
                    self.accept_completion(ctx, accepted)?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.disactivate();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::SHIFT,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.previous();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.menu.next();
            },
            _ => {
                self.menu.disactivate();
                match ctx.mode {
                    LineMode::Insert => {
                        self.handle_insert_keys(ctx, event)?;
                    },
                    LineMode::Normal => {
                        self.handle_normal_keys(ctx, event)?;
                    },
                };
            },
        };
        Ok(())
    }
    fn needs_multiline(&self, ctx: &mut LineCtx) -> bool {
        //TODO check if open quotes or brackets

        if let Some(last_char) = ctx
            .cb
            .char_at(Location::Abs(ctx.cb.len().saturating_sub(1)))
        {
            if last_char == '\\' {
                return true;
            }
        };

        let mut brackets: Vec<Token> = vec![];
        let command = self.get_full_command(ctx);
        let lexer = Lexer::new(command.as_str());

        for t in lexer {
            if let Ok(token) = t {
                match token.1 {
                    Token::LBRACE => brackets.push(token.1),
                    Token::LPAREN => brackets.push(token.1),
                    Token::RPAREN => {
                        if let Some(bracket) = brackets.last() {
                            if bracket == &Token::LPAREN {
                                brackets.pop();
                            } else {
                                return false;
                            }
                        }
                    },
                    Token::RBRACE => {
                        if let Some(bracket) = brackets.last() {
                            if bracket == &Token::LBRACE {
                                brackets.pop();
                            } else {
                                return false;
                            }
                        }
                    },
                    Token::WORD(w) => {
                        if let Some(c) = w.chars().next() {
                            if c == '\'' {
                                if w.len() == 1 {
                                    return true;
                                }
                                if let Some(e) = w.chars().last() {
                                    return e != '\'';
                                } else {
                                    return true;
                                }
                            }
                            if c == '\"' {
                                if w.len() == 1 {
                                    return true;
                                }

                                if let Some(e) = w.chars().last() {
                                    return e != '\"';
                                } else {
                                    return true;
                                }
                            }
                        }
                    },

                    _ => (),
                }
            }
        }

        !brackets.is_empty()
    }
    fn get_full_command(&self, ctx: &mut LineCtx) -> String {
        let mut res: String = ctx.lines.clone();
        let cur_line: String = ctx.cb.as_str().into();
        res += cur_line.as_str();

        res
    }
    //Keys that are universal regardless of mode, ex. Enter, Ctrl-c
    fn handle_standard_keys(&mut self, ctx: &mut LineCtx, event: Event) -> anyhow::Result<bool> {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if self.menu.is_active() {
                    return Ok(false);
                }
                self.buffer_history.clear();
                self.painter.newline()?;

                if self.needs_multiline(ctx) {
                    ctx.lines += ctx.cb.as_str().into_owned().as_str();
                    ctx.lines += "\n";
                    ctx.cb.clear();

                    return Ok(false);
                }

                return Ok(true);
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                // if current input is empty exit the shell, otherwise treat it as enter
                if ctx.cb.len() == 0 {
                    // TODO maybe unify exiting the shell
                    disable_raw_mode(); // TODO this is temp fix, should be more graceful way of
                                        // handling cleanup code
                    std::process::exit(0);
                } else {
                    self.buffer_history.clear();
                    self.painter.newline()?;
                    return Ok(true);
                }
            },

            _ => (),
        };

        Ok(false)
    }

    fn handle_insert_keys(&mut self, ctx: &mut LineCtx, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.populate_completions(ctx)?;
                self.menu.activate();

                // if completions only has one entry, automatically select it
                let completion_len = self.menu.items().len();
                if completion_len == 1 {
                    // TODO stupid ownership stuff
                    let item = self.menu.items().get(0).map(|x| (*x).clone()).unwrap();
                    self.accept_completion(ctx, item.1)?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if ctx.cb.cursor() > 0 {
                    ctx.cb.move_cursor(Location::Before())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                if ctx.cb.cursor() < ctx.cb.len() {
                    ctx.cb.move_cursor(Location::After())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.history_down(ctx)?;
            },
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                self.history_up(ctx)?;
            },
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                self.to_normal_mode(ctx)?;
                self.buffer_history.add(&ctx.cb);
            },
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                modifiers: KeyModifiers::NONE,
                ..
            })
            | Event::Key(KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if ctx.cb.len() > 0 && ctx.cb.cursor() != 0 {
                    ctx.cb.delete(Location::Before(), Location::Cursor())?;
                }
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                if ctx.cb.len() > 0 && ctx.cb.cursor() != 0 {
                    let start = ctx.cb.motion_to_loc(Motion::BackWord)?;
                    ctx.cb.delete(start, Location::Cursor())?;
                }
            },

            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                ctx.cb.insert(Location::Cursor(), &c.to_string())?;
            },
            _ => {},
        };
        Ok(())
    }

    fn handle_normal_keys(&mut self, ctx: &mut LineCtx, event: Event) -> anyhow::Result<()> {
        // TODO write better system toString support key combinations
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                self.normal_keys.clear();
            },
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                self.normal_keys.push(c);

                if let Ok(Command { repeat, action }) = Parser::new().parse(&self.normal_keys) {
                    for _ in 0..repeat {
                        // special cases (possibly consulidate with execute_vi somehow)

                        if let Ok(mode) = ctx.cb.execute_vi(action.clone()) {
                            match mode {
                                LineMode::Insert => self.to_insert_mode(ctx)?,
                                LineMode::Normal => self.to_normal_mode(ctx)?,
                            };
                        }
                        match action {
                            Action::Undo => self.buffer_history.prev(ctx.cb.borrow_mut()),

                            Action::Redo => self.buffer_history.next(ctx.cb.borrow_mut()),
                            Action::Move(motion) => match motion {
                                Motion::Up => self.history_up(ctx)?,
                                Motion::Down => self.history_down(ctx)?,
                                _ => {},
                            },
                            _ => {
                                self.buffer_history.add(&ctx.cb);
                            },
                        }
                    }

                    self.normal_keys.clear();
                }
            },
            _ => {},
        }
        Ok(())
    }

    // recalculate the current completions
    fn populate_completions(&mut self, ctx: &mut LineCtx) -> anyhow::Result<()> {
        // TODO IFS
        let args = ctx.cb.slice(..ctx.cb.cursor()).as_str().unwrap().split(' ');
        ctx.current_word = args.clone().last().unwrap_or("").to_string();

        let comp_ctx = CompletionCtx::new(args.map(|s| s.to_owned()).collect::<Vec<_>>());

        let completions = self.completer.complete(&comp_ctx);
        let completions = completions
            .iter()
            .take(10) // TODO make this config
            .collect::<Vec<_>>();

        let menuitems = completions
            .iter()
            .map(|c| (c.display(), (*c).clone()))
            .collect::<Vec<_>>();
        self.menu.set_items(menuitems);

        Ok(())
    }

    // replace word at cursor with accepted word (used in automcompletion)
    fn accept_completion(
        &mut self,
        ctx: &mut LineCtx,
        completion: Completion,
    ) -> anyhow::Result<()> {
        let accepted = if let Some(accepted) = self.menu.accept().cloned() {
            accepted
        } else {
            return Ok(());
        };

        // first remove current word
        // TODO could implement a delete_before
        // TODO make use of ReplaceMethod
        ctx.cb
            .move_cursor(Location::Rel(-(ctx.current_word.len() as isize)))?;

        let cur_word_len = unicode_width::UnicodeWidthStr::width(ctx.current_word.as_str());
        ctx.cb
            .delete(Location::Cursor(), Location::Rel(cur_word_len as isize))?;

        ctx.current_word.clear();

        // then replace with the completion word
        ctx.cb.insert(Location::Cursor(), &accepted.accept())?;

        Ok(())
    }

    fn history_up(&mut self, ctx: &mut LineCtx) -> anyhow::Result<()> {
        // save current prompt
        if HistoryInd::Prompt == ctx.history_ind {
            ctx.saved_line = ctx.cb.slice(..).to_string();
        }

        ctx.history_ind = ctx.history_ind.up(self.history.len());
        self.update_history(ctx)?;

        Ok(())
    }

    fn history_down(&mut self, ctx: &mut LineCtx) -> anyhow::Result<()> {
        ctx.history_ind = ctx.history_ind.down();
        self.update_history(ctx)?;

        Ok(())
    }

    fn update_history(&mut self, ctx: &mut LineCtx) -> anyhow::Result<()> {
        match ctx.history_ind {
            // restore saved line
            HistoryInd::Prompt => {
                ctx.cb.clear();
                ctx.cb.insert(Location::Cursor(), &ctx.saved_line)?;
            },
            // fill prompt with history element
            HistoryInd::Line(i) => {
                let history_item = self.history.get(i).unwrap();
                ctx.cb.clear();
                ctx.cb.insert(Location::Cursor(), history_item)?;
            },
        }
        Ok(())
    }

    fn to_normal_mode(&mut self, line_ctx: &mut LineCtx) -> anyhow::Result<()> {
        line_ctx
            .ctx
            .state
            .get_mut::<CursorStyle>()
            .map(|cursor_style| cursor_style.style = SetCursorStyle::BlinkingBlock);
        line_ctx.mode = LineMode::Normal;

        let hook_ctx = LineModeSwitchCtx {
            line_mode: LineMode::Normal,
        };
        line_ctx.sh.hooks.run::<LineModeSwitchCtx>(
            line_ctx.sh,
            line_ctx.ctx,
            line_ctx.rt,
            hook_ctx,
        )?;
        Ok(())
    }

    fn to_insert_mode(&mut self, line_ctx: &mut LineCtx) -> anyhow::Result<()> {
        line_ctx
            .ctx
            .state
            .get_mut::<CursorStyle>()
            .map(|cursor_style| cursor_style.style = SetCursorStyle::BlinkingBar);
        line_ctx.mode = LineMode::Insert;

        let hook_ctx = LineModeSwitchCtx {
            line_mode: LineMode::Insert,
        };
        line_ctx.sh.hooks.run::<LineModeSwitchCtx>(
            line_ctx.sh,
            line_ctx.ctx,
            line_ctx.rt,
            hook_ctx,
        )?;
        Ok(())
    }
}
