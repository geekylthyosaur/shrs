//! Shell runtime hooks
//!
//! Hooks are user provided functions that are called on a variety of events that occur in the
//! shell. Some additional context is provided to these hooks.
// ideas for hooks
// - on start
// - after prompt
// - before prompt
// - internal error hook (call whenever there is internal shell error; good for debug)
// - env hook (when envrionment variable is set/changed)
// - exit hook (tricky, make sure we know what cases to call this)

use std::{
    any::{Any, TypeId},
    collections::HashMap,
    io::BufWriter,
    marker::PhantomData,
    path::PathBuf,
    time::Duration,
};

use crossterm::{style::Print, QueueableCommand};

use crate::{jobs::ExitStatus, Context, Runtime, Shell};

pub type HookFn<C: Clone> =
    fn(sh: &Shell, sh_ctx: &mut Context, sh_rt: &mut Runtime, ctx: &C) -> anyhow::Result<()>;

// TODO this is some pretty sus implementation
pub trait Hook<C>: FnMut(&Shell, &mut Context, &mut Runtime, &C) -> anyhow::Result<()> {}

impl<C, T: FnMut(&Shell, &mut Context, &mut Runtime, &C) -> anyhow::Result<()>> Hook<C> for T {}

/// Context for [StartupHook]
#[derive(Clone)]
pub struct StartupCtx {
    pub startup_time: Duration,
}

/// Default [StartupHook]
pub fn startup_hook(
    sh: &Shell,
    sh_ctx: &mut Context,
    sh_rt: &mut Runtime,
    _ctx: &StartupCtx,
) -> anyhow::Result<()> {
    println!("welcome to shrs!");
    Ok(())
}

/// Context for [BeforeCommandHook]
#[derive(Clone)]
pub struct BeforeCommandCtx {
    /// Literal command entered by user
    pub raw_command: String,
    /// Command to be executed, after performing all substitutions
    pub command: String,
}
/// Default [BeforeCommandHook]
pub fn before_command_hook(
    sh: &Shell,
    sh_ctx: &mut Context,
    sh_rt: &mut Runtime,
    ctx: &BeforeCommandCtx,
) -> anyhow::Result<()> {
    // let expanded_cmd = format!("[evaluating] {}\n", ctx.command);
    // out.queue(Print(expanded_cmd))?;
    Ok(())
}

/// Context for [AfterCommandHook]
#[derive(Clone)]
pub struct AfterCommandCtx {
    /// Exit code of previous command
    pub exit_code: i32,
    /// Amount of time it took to run command
    pub cmd_time: f32,
    /// Command output
    pub cmd_output: String,
}

/// Default [AfterCommandHook]
pub fn after_command_hook(
    sh: &Shell,
    sh_ctx: &mut Context,
    sh_rt: &mut Runtime,
    ctx: &AfterCommandCtx,
) -> anyhow::Result<()> {
    // let exit_code_str = format!("[exit +{}]\n", ctx.exit_code);
    // out.queue(Print(exit_code_str))?;
    Ok(())
}

/// Context for [ChangeDirHook]
#[derive(Clone)]
pub struct ChangeDirCtx {
    pub old_dir: PathBuf,
    pub new_dir: PathBuf,
}

/// Default [AfterCommandHook]
pub fn change_dir_hook(
    sh: &Shell,
    sh_ctx: &mut Context,
    sh_rt: &mut Runtime,
    ctx: &ChangeDirCtx,
) -> anyhow::Result<()> {
    Ok(())
}

/// Context for [JobExit]
#[derive(Clone)]
pub struct JobExitCtx {
    pub status: ExitStatus,
}

/// Default [JobExitHook]
pub fn job_exit_hook(
    sh: &Shell,
    sh_ctx: &mut Context,
    sh_rt: &mut Runtime,
    ctx: &JobExitCtx,
) -> anyhow::Result<()> {
    println!("[exit +{}]", ctx.status.code());
    Ok(())
}

/// Collection of all the hooks that are available
pub struct Hooks {
    // TODO how to uniquely identify a hook? using the Ctx type?
    hooks: anymap::Map,
}

impl Default for Hooks {
    fn default() -> Self {
        let mut hooks = Hooks::new();

        hooks.register(startup_hook);
        hooks.register(before_command_hook);
        hooks.register(after_command_hook);
        hooks.register(change_dir_hook);
        hooks.register(job_exit_hook);

        hooks
    }
}

impl Hooks {
    pub fn new() -> Self {
        Self {
            hooks: anymap::Map::new(),
        }
    }

    /// Registers a new hook
    pub fn register<C: Clone + 'static>(&mut self, hook: HookFn<C>) {
        match self.hooks.get_mut::<Vec<HookFn<C>>>() {
            Some(hook_list) => {
                hook_list.push(hook);
            },
            None => {
                // register any empty vector for the type
                self.hooks.insert::<Vec<HookFn<C>>>(vec![hook]);
            },
        };
    }

    /// Register from an iterator
    pub fn register_iter(&mut self) {}

    /// Executes all registered hooks
    pub fn run<C: Clone + 'static>(
        &self,
        sh: &Shell,
        sh_ctx: &mut Context,
        sh_rt: &mut Runtime,
        ctx: C,
    ) -> anyhow::Result<()> {
        if let Some(hook_list) = self.hooks.get::<Vec<HookFn<C>>>() {
            for hook in hook_list.iter() {
                (hook)(sh, sh_ctx, sh_rt, &ctx)?;
            }
        }
        Ok(())
    }
}
