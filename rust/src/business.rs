//! 不依赖 Qt 的业务示例；后续实际业务从这里开始拆分。

/// 去除输入首尾空白并生成结果；空输入返回可显示的错误，不访问 UI。
pub fn process_message(input: &str) -> Result<String, String> {
    let message = input.trim();
    if message.is_empty() {
        return Err("请输入内容".to_owned());
    }
    Ok(format!("Rust 已处理：{message}"))
}

#[cfg(test)]
mod tests {
    use super::process_message;

    /// 验证跨语言常用的中文输入和空白归一化规则。
    #[test]
    fn processes_unicode() {
        assert_eq!(
            process_message(" 中文验证 ").unwrap(),
            "Rust 已处理：中文验证"
        );
    }

    /// 空白输入应返回错误，防止示例把失败误报为成功。
    #[test]
    fn rejects_blank_input() {
        assert!(process_message(" \n ").is_err());
    }
}
