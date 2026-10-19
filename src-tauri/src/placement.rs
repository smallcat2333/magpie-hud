//! Windows 显示器工作区与悬浮窗边界计算。
//!
//! 输入坐标统一使用物理像素；调用 Tauri 前再按目标显示器缩放转换为逻辑像素。
//! 约束目标是整窗位于任务栏之外的工作区，支持负坐标和小于窗口的工作区。

/// 物理像素矩形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalRect {
    /// 左边界。
    pub x: i32,
    /// 上边界。
    pub y: i32,
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
}

/// 选择与窗口重叠面积最大的显示器；没有重叠时选择距离窗口中心最近的工作区。
pub fn monitor_for_window(window: PhysicalRect, monitors: &[PhysicalRect]) -> Option<PhysicalRect> {
    let center_x = window.x.saturating_add(window.width as i32 / 2);
    let center_y = window.y.saturating_add(window.height as i32 / 2);
    monitors.iter().copied().max_by(|first, second| overlap_area(window, *first).cmp(&overlap_area(window, *second)).then_with(|| contains(first, center_x, center_y).cmp(&contains(second, center_x, center_y)))).filter(|monitor| overlap_area(window, *monitor) > 0).or_else(|| monitors.iter().copied().min_by_key(|monitor| distance_squared(monitor, center_x, center_y)))
}

/// 把窗口限制在工作区内。
///
/// 工作区足够大时只移动位置；窗口更大时从工作区原点开始并缩小到可用边界。
pub fn constrain(window: PhysicalRect, work_area: PhysicalRect) -> PhysicalRect {
    let width = window.width.min(work_area.width.max(1));
    let height = window.height.min(work_area.height.max(1));
    let max_x = work_area.x.saturating_add(work_area.width.saturating_sub(width) as i32);
    let max_y = work_area.y.saturating_add(work_area.height.saturating_sub(height) as i32);
    PhysicalRect { x: window.x.clamp(work_area.x, max_x), y: window.y.clamp(work_area.y, max_y), width, height }
}

/// 判断点是否位于矩形内，右边界和下边界不包含。
fn contains(rect: &PhysicalRect, x: i32, y: i32) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x.saturating_add(rect.width as i32) && y < rect.y.saturating_add(rect.height as i32)
}
fn overlap_area(first: PhysicalRect, second: PhysicalRect) -> u64 {
    let left = first.x.max(second.x);
    let top = first.y.max(second.y);
    let right = first.x.saturating_add(first.width as i32).min(second.x.saturating_add(second.width as i32));
    let bottom = first.y.saturating_add(first.height as i32).min(second.y.saturating_add(second.height as i32));
    if right <= left || bottom <= top { 0 } else { (right - left) as u64 * (bottom - top) as u64 }
}

/// 计算点到矩形的平方距离；矩形内距离为 0。
fn distance_squared(rect: &PhysicalRect, x: i32, y: i32) -> i64 {
    let dx = if x < rect.x { rect.x - x } else { (x - rect.x.saturating_add(rect.width as i32)).max(0) };
    let dy = if y < rect.y { rect.y - y } else { (y - rect.y.saturating_add(rect.height as i32)).max(0) };
    dx as i64 * dx as i64 + dy as i64 * dy as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证负坐标、跨屏选择、任务栏工作区和小工作区缩小。
    #[test]
    fn clamps_negative_and_small_work_areas() {
        let left = PhysicalRect { x: -1920, y: 0, width: 1920, height: 1040 };
        let right = PhysicalRect { x: 0, y: 0, width: 1920, height: 1000 };
        let window = PhysicalRect { x: -300, y: 700, width: 840, height: 280 };
        assert_eq!(monitor_for_window(window, &[left, right]), Some(right));
        assert_eq!(constrain(window, right), PhysicalRect { x: 0, y: 700, width: 840, height: 280 });
        assert_eq!(constrain(window, PhysicalRect { x: 10, y: 20, width: 500, height: 100 }), PhysicalRect { x: 10, y: 20, width: 500, height: 100 });
    }
}
