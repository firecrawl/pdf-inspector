//! Пересборка строк по порядку чтения из дерева структуры.
//!
//! ISO 32000-1 §14.8.2.3: последовательность чтения объявляет автор документа,
//! а не расположение на странице. Геометрия знает лишь то, что два куска текста
//! стоят на одной высоте, — но не то, читаются они подряд или лежат в разных
//! колонках. Отсюда и берётся классическая беда: телефон из левой колонки
//! влезает в середину предложения из правой.
//!
//! Здесь строки собираются заново: элементы раскладываются в порядке дерева, и
//! в одну строку попадают только соседние по чтению. Колонки при этом
//! расходятся сами — их куски не соседи в дереве, как бы близко они ни стояли
//! на странице.
//!
//! Если дерева нет или оно покрывает текст не полностью, всё остаётся как было:
//! неполный порядок хуже честной геометрии.

use crate::types::{TextItem, TextLine};
use std::collections::HashMap;

/// Карта «страница → метка содержимого → номер в порядке чтения».
pub(crate) type ReadingOrder = HashMap<u32, HashMap<i64, usize>>;

/// Доля элементов, которым дерево даёт номер.
///
/// Ниже этой доли пересборка не включается: смешивать объявленный порядок с
/// догадками — значит получить third вариант, неверный по-своему.
const REQUIRED_COVERAGE: f32 = 0.98;

pub(crate) fn regroup(lines: Vec<TextLine>, order: &ReadingOrder) -> Vec<TextLine> {
    if order.is_empty() {
        return lines;
    }

    let total: usize = lines.iter().map(|l| l.items.len()).sum();
    if total == 0 {
        return lines;
    }

    let rank_of = |item: &TextItem| -> Option<usize> {
        order.get(&item.page)?.get(&item.mcid?).copied()
    };

    let covered = lines
        .iter()
        .flat_map(|l| l.items.iter())
        .filter(|i| rank_of(i).is_some())
        .count();
    if (covered as f32) < total as f32 * REQUIRED_COVERAGE {
        return lines;
    }

    // Порог склейки берётся от исходных строк: он вычислен по этой странице и
    // заново его выводить неоткуда.
    let threshold_for: HashMap<u32, f32> = lines
        .iter()
        .map(|l| (l.page, l.adaptive_threshold))
        .collect();

    let mut ranked: Vec<(usize, TextItem)> = lines
        .into_iter()
        .flat_map(|l| l.items)
        .filter_map(|i| rank_of(&i).map(|r| (r, i)))
        .collect();

    // Порядок дерева — главный ключ. Внутри одного куска содержимого элементы
    // упорядочиваются как обычно, сверху вниз.
    ranked.sort_by(|a, b| {
        a.1.page
            .cmp(&b.1.page)
            .then(a.0.cmp(&b.0))
            .then(b.1.y.total_cmp(&a.1.y))
    });

    let mut out: Vec<TextLine> = Vec::new();
    for (_, item) in ranked {
        let threshold = threshold_for.get(&item.page).copied().unwrap_or(2.0);
        let same_line = out.last().is_some_and(|last: &TextLine| {
            last.page == item.page && (last.y - item.y).abs() <= threshold
        });
        if same_line {
            let last = out.last_mut().unwrap();
            last.items.push(item);
        } else {
            out.push(TextLine {
                y: item.y,
                page: item.page,
                adaptive_threshold: threshold,
                items: vec![item],
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn item(text: &str, x: f32, y: f32, mcid: i64) -> TextItem {
        TextItem {
            text: text.into(),
            x,
            y,
            width: 20.0,
            height: 12.0,
            font: "F1".into(),
            font_size: 12.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: ItemType::Text,
            mcid: Some(mcid),
        }
    }

    fn line(items: Vec<TextItem>) -> TextLine {
        TextLine {
            y: items[0].y,
            page: 1,
            adaptive_threshold: 2.0,
            items,
        }
    }

    /// Две колонки на одной высоте: геометрия сшивает их в строку, дерево
    /// разводит. Левая колонка объявлена второй, поэтому и читается второй.
    #[test]
    fn columns_are_split_by_the_tree() {
        let lines = vec![line(vec![
            item("телефон", 50.0, 700.0, 10),
            item("основной", 300.0, 700.0, 1),
        ])];
        let mut order = HashMap::new();
        let mut page = HashMap::new();
        page.insert(1i64, 0usize); // правая колонка читается первой
        page.insert(10i64, 5usize);
        order.insert(1u32, page);

        let out = regroup(lines, &order);
        let texts: Vec<&str> = out
            .iter()
            .flat_map(|l| l.items.iter())
            .map(|i| i.text.as_str())
            .collect();
        assert_eq!(texts, vec!["основной", "телефон"]);
    }

    /// Дерево, покрывающее текст частично, не применяется вовсе.
    #[test]
    fn partial_coverage_falls_back_to_geometry() {
        let mut a = item("раз", 10.0, 700.0, 1);
        a.mcid = None;
        let lines = vec![line(vec![a, item("два", 100.0, 700.0, 2)])];
        let before = lines.clone();
        let mut order = HashMap::new();
        let mut page = HashMap::new();
        page.insert(2i64, 0usize);
        order.insert(1u32, page);

        let out = regroup(lines, &order);
        assert_eq!(out.len(), before.len());
        assert_eq!(out[0].items.len(), 2);
    }
}
