const messages: Record<string, string> = {
  picker_busy: '已有一个文件夹选择窗口，请先完成或取消。', picker_timeout: '目录选择已超时，请重新选择或输入路径。', picker_unavailable: '暂时无法打开系统窗口，可以直接输入路径。', picker_unsupported: '当前系统暂不支持目录窗口，请直接输入路径。', picker_invalid_result: '未能取得有效目录，请重新选择或输入路径。',
  project_unavailable: '目录不存在或无法读取，请检查项目位置。', invalid_project_path: '请输入有效的本机目录。', absolute_path_required: '请填写完整目录，或使用 ~/ 开头的路径。',
  native_home_unavailable: '原生 Codex 历史目录不可用，请先检查本机 CLI 配置。', native_launch_failed: 'Codex 未能启动，请检查已安装程序及原生模型配置。',
  project_changed: '目录在检查后发生变化，请重新检查。', native_home_changed: '原生历史位置发生变化，请重新检查。', config_changed: '设置已经变化，请重新检查后启动。',
  target_expired: '目录检查已过期，请重新检查。', settings_invalid: '工作台设置有误，请在设置中检查。', settings_unavailable: '设置暂时无法读取，请重试。',
  resume_lookup_unsupported: '此会话存在重复或不受支持的历史文件，暂不能从网页恢复，仍可阅读。', resume_lookup_limit: '原生历史目录较大，本次未能完成恢复检查，请稍后重试或使用原生 CLI。', run_stopping: '之前的 CLI 正在结束，请稍后重新检查目录。',
  resume_unavailable: '这条记录没有可核验的原生恢复入口，仍可继续阅读。', resume_history_unsupported: '这条会话包含尚未验证的继承历史，暂不能从此处恢复。',
  resume_source_unavailable: '原生会话文件已不存在或不可读，暂不能恢复。', resume_identity_changed: '原生会话身份或目录已变化，请更新历史后重试。',
  source_revision_changed: '历史文件已经变化，请更新历史后重新选择会话。', source_revoked: '历史来源已移除或改变，请更新历史。', entry_unavailable: '这条原生记录已不可用，请更新历史。',
  project_session_running: '该项目已有另一会话，请进入已有工作台，或停止后再继续此会话。', project_already_running: '已有工作台正在运行，请进入已有工作台，或停止后重新检查目录。',
  run_capacity: '已达到同时运行的项目上限，请先停止一个工作台，再重新检查目录。',
  native_session_running: '这条原生会话已在另一个工作台运行，请进入已有工作台，或停止后再继续。',
  launch_busy: '另一个启动或目录检查正在处理，请稍后重试。', operation_unavailable: '这次启动记录已失效或应用已重启。请查看已有工作台；系统不会自动再启动。',
  invalid_instance: '应用已经重新启动，请重新打开首页。', pairing_required: '连接授权已失效，请重新打开配对入口。', operation_conflict: '启动操作标识冲突，请重新打开首页。',
  native_cli_not_found: '未找到可运行的 Codex CLI，请检查安装位置及启动工作台时的 PATH。',
  native_cli_version_failed: '无法读取 Codex CLI 版本，请在本机终端检查 codex --version 是否正常。',
  native_config_invalid: '原生 Codex 配置无法读取或解析，请检查配置文件的格式和读取权限。',
  native_provider_unsupported: '当前模型提供方配置暂不支持工作台代理，请检查所选提供方及其连接配置。',
  native_auth_unverified: '当前认证方式尚未通过工作台验证，请在原生 CLI 中核验登录与认证配置；如受组织策略管理，请联系管理员。',
  native_project_config_invalid: '项目中的 Codex 配置无法用于本次启动，请检查项目配置及原生 CLI 的配置诊断。',
  native_recording_unavailable: '无法初始化工作台历史记录，请检查工作台数据目录的写入权限和磁盘空间。',
  native_proxy_unavailable: '无法启动本机模型代理，请检查本机网络监听权限后重试。',
  native_terminal_unavailable: '无法创建 Codex 终端，请检查本机终端环境和进程权限后重试。',
  native_workspace_unavailable: '无法初始化项目工作区，请检查项目目录的读取权限后重试。',
  native_config_changed: '原生 Codex 配置在检查后发生变化，请重新检查目录后启动。',
};

export function launchMessage(code: unknown): string {
  return typeof code === 'string' && Object.prototype.hasOwnProperty.call(messages, code)
    ? messages[code]
    : '操作未能完成，请检查连接或重新检查目录。';
}
