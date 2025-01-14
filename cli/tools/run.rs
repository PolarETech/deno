// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::io::Read;

use deno_ast::MediaType;
use deno_ast::ModuleSpecifier;
use deno_core::error::AnyError;
use deno_runtime::permissions::Permissions;
use deno_runtime::permissions::PermissionsContainer;

use crate::args::EvalFlags;
use crate::args::Flags;
use crate::file_fetcher::File;
use crate::proc_state::ProcState;
use crate::util;

pub async fn run_script(flags: Flags) -> Result<i32, AnyError> {
  if !flags.has_permission() && flags.has_permission_in_argv() {
    log::warn!(
      "{}",
      crate::colors::yellow(
        r#"Permission flags have likely been incorrectly set after the script argument.
To grant permissions, set them before the script argument. For example:
    deno run --allow-read=. main.js"#
      )
    );
  }

  if flags.watch.is_some() {
    return run_with_watch(flags).await;
  }

  // TODO(bartlomieju): actually I think it will also fail if there's an import
  // map specified and bare specifier is used on the command line - this should
  // probably call `ProcState::resolve` instead
  let ps = ProcState::from_flags(flags).await?;

  // Run a background task that checks for available upgrades. If an earlier
  // run of this background task found a new version of Deno.
  super::upgrade::check_for_upgrades(
    ps.http_client.clone(),
    ps.dir.upgrade_check_file_path(),
  );

  let main_module = ps.options.resolve_main_module()?;

  let permissions = PermissionsContainer::new(Permissions::from_options(
    &ps.options.permissions_options(),
  )?);
  let worker_factory = ps.into_cli_main_worker_factory();
  let mut worker = worker_factory
    .create_main_worker(main_module, permissions)
    .await?;

  let exit_code = worker.run().await?;
  Ok(exit_code)
}

pub async fn run_from_stdin(flags: Flags) -> Result<i32, AnyError> {
  let ps = ProcState::from_flags(flags).await?;
  let main_module = ps.options.resolve_main_module()?;

  let permissions = PermissionsContainer::new(Permissions::from_options(
    &ps.options.permissions_options(),
  )?);
  let mut source = Vec::new();
  std::io::stdin().read_to_end(&mut source)?;
  // Create a dummy source file.
  let source_file = File {
    local: main_module.clone().to_file_path().unwrap(),
    maybe_types: None,
    media_type: MediaType::TypeScript,
    source: String::from_utf8(source)?.into(),
    specifier: main_module.clone(),
    maybe_headers: None,
  };
  // Save our fake file into file fetcher cache
  // to allow module access by TS compiler
  ps.file_fetcher.insert_cached(source_file);

  let worker_factory = ps.into_cli_main_worker_factory();
  let mut worker = worker_factory
    .create_main_worker(main_module, permissions)
    .await?;
  let exit_code = worker.run().await?;
  Ok(exit_code)
}

// TODO(bartlomieju): this function is not handling `exit_code` set by the runtime
// code properly.
async fn run_with_watch(flags: Flags) -> Result<i32, AnyError> {
  let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
  let mut ps =
    ProcState::from_flags_for_file_watcher(flags, sender.clone()).await?;
  let clear_screen = !ps.options.no_clear_screen();
  let main_module = ps.options.resolve_main_module()?;

  let operation = |main_module: ModuleSpecifier| {
    ps.reset_for_file_watcher();
    let ps = ps.clone();
    Ok(async move {
      let permissions = PermissionsContainer::new(Permissions::from_options(
        &ps.options.permissions_options(),
      )?);
      let worker_factory = ps.into_cli_main_worker_factory();
      let worker = worker_factory
        .create_main_worker(main_module, permissions)
        .await?;
      worker.run_for_watcher().await?;

      Ok(())
    })
  };

  util::file_watcher::watch_func2(
    receiver,
    operation,
    main_module,
    util::file_watcher::PrintConfig {
      job_name: "Process".to_string(),
      clear_screen,
    },
  )
  .await?;

  Ok(0)
}

pub async fn eval_command(
  flags: Flags,
  eval_flags: EvalFlags,
) -> Result<i32, AnyError> {
  let ps = ProcState::from_flags(flags).await?;
  let main_module = ps.options.resolve_main_module()?;
  let permissions = PermissionsContainer::new(Permissions::from_options(
    &ps.options.permissions_options(),
  )?);
  // Create a dummy source file.
  let source_code = if eval_flags.print {
    format!("console.log({})", eval_flags.code)
  } else {
    eval_flags.code
  }
  .into_bytes();

  let file = File {
    local: main_module.clone().to_file_path().unwrap(),
    maybe_types: None,
    media_type: MediaType::Unknown,
    source: String::from_utf8(source_code)?.into(),
    specifier: main_module.clone(),
    maybe_headers: None,
  };

  // Save our fake file into file fetcher cache
  // to allow module access by TS compiler.
  ps.file_fetcher.insert_cached(file);

  let mut worker = ps
    .into_cli_main_worker_factory()
    .create_main_worker(main_module, permissions)
    .await?;
  let exit_code = worker.run().await?;
  Ok(exit_code)
}
