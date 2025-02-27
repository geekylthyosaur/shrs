use shrs::{
    crossterm::Stylize,
    line::{LineBuilder, LineCtx, Prompt, StyledBuf},
    ShellConfigBuilder,
};
use shrs_cd_tools::git;

struct MyPrompt;

impl Prompt for MyPrompt {
    fn prompt_left(&self, line_ctx: &mut LineCtx) -> StyledBuf {
        StyledBuf::from_iter(vec![String::from(" > ").reset()])
    }
    fn prompt_right(&self, line_ctx: &mut LineCtx) -> StyledBuf {
        let branch: String = git::branch().unwrap_or_default();
        StyledBuf::from_iter(vec![branch.bold().reset()])
    }
}

fn main() {
    let readline = LineBuilder::default()
        .with_prompt(MyPrompt)
        .build()
        .unwrap();

    let myshell = ShellConfigBuilder::default()
        .with_readline(readline)
        .build()
        .unwrap();

    myshell.run();
}
