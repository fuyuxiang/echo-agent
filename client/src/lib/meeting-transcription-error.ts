/** Connection/credential errors need user action; reconnecting the network
 * cannot repair them. Keep the native reason available in the UI. */
export function meetingTranscriptionIssue(error?: string): { settings: boolean; message: string } {
  const settings=!!error && /\b(?:401|402|403|404|405)\b|API\s*Key|Base\s*URL|Bearer|Chat Completions|模型.*(?:不存在|变化|不是|不提供|未配置)|连接.*(?:删除|未配置)|配置.*(?:过期|同步)|凭据|密钥|鉴权|授权|余额|额度|配额|quota|insufficient|unauthorized|forbidden|invalid.*(?:key|token)|authentication/i.test(error);
  return settings
    ? {settings:true,message:'转写连接需要处理，录音继续保存在本机。请检查连接、凭据或服务额度，处理后重试转写。'}
    : {settings:false,message:'短句转写暂不可用，录音继续保存在本机；服务或网络恢复后会自动重试。'};
}
