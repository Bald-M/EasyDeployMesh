<!--
标题使用 Conventional Commits：<type>(<scope>): <summary>，例如 fix(pxe): fail incomplete TFTP transfers
Title uses Conventional Commits: <type>(<scope>): <summary>
-->

<!-- 问题与最终行为，一两段即可 / The problem and the resulting behavior, in a paragraph or two -->

Closes #

## Verification

<!-- 列出实际运行的命令和结果，没跑的写明原因；只写真实结果 / Commands actually run and their results; say why anything was skipped -->

- `pnpm check`:

<!--
涉及 Windows、WinPE、PXE、分区或其他破坏性流程时，写明在哪台一次性虚拟机或机器上、按什么步骤验证；可见 UI 改动附截图。
For Windows, WinPE, PXE, partitioning, or other destructive workflows, describe the disposable VM or machine and the manual steps; attach screenshots for visible UI changes.
-->

## Risk and compatibility

<!--
安全不变量、协议或持久化格式、WinPE 运行时布局版本、迁移与回滚等影响；没有则写 None。
Effects on safety invariants, protocol or persisted formats, WinPE runtime layout revision, migration, and rollback; write None if not applicable.
-->
