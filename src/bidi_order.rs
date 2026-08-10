//! Восстановление логического порядка по стандарту UAX #9.
//!
//! PDF хранит текст справа налево в визуальном порядке: внутри одной операции
//! показа перо идёт слева направо по ширинам глифов, поэтому иначе страница
//! отрисовалась бы зеркально. Извлекателю нужен обратный переход — из
//! визуального в логический.
//!
//! Раньше здесь была самодельная эвристика с порогом «латиницы вдвое больше».
//! Она подгонялась под очередной файл и ломалась на следующем: строка
//! `Hello של world` и строка `שירותי מחשוב System Administrator` имеют почти
//! одинаковые пропорции, а вести себя должны противоположно. Информации внутри
//! строки для этого решения просто нет.
//!
//! Стандарт решает это иначе: направление задаётся **уровнем абзаца**, а не
//! подсчётом букв. Правило P2/P3 берёт его от первого сильного символа в
//! логическом порядке — которого у нас как раз нет, — но UAX #9 прямо разрешает
//! протоколу более высокого уровня задать его самому (HL1). Наш протокол:
//! направление документа, посчитанное по всему извлечённому тексту. На уровне
//! документа сигнал устойчив — ивритское резюме даёт 1210 ивритских букв против
//! 516 латинских, английское 0 против 1436, — тогда как на уровне строки он
//! шумит.
//!
//! Дальше работает сам алгоритм: классы символов, слабые типы, нейтральные,
//! уровни и перестановка L2. Всё то, что я писал руками и в чём ошибался —
//! огласовки, арабо-индийские цифры, не-ASCII латиница, — там уже учтено.

use unicode_bidi::{bidi_class, BidiClass, BidiInfo, Level};

/// Направление письма документа.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Direction {
    Ltr,
    Rtl,
}

impl Direction {
    fn level(self) -> Level {
        match self {
            Direction::Ltr => Level::ltr(),
            Direction::Rtl => Level::rtl(),
        }
    }
}

/// Направление по преобладанию сильных символов.
///
/// Считаются только буквы: цифры и пунктуация направления не несут, а
/// латинских названий систем в ивритском резюме столько, что счёт по всем
/// символам переворачивал бы вывод.
pub(crate) fn dominant_direction<'a, I>(texts: I) -> Direction
where
    I: Iterator<Item = &'a str>,
{
    let (mut rtl, mut ltr) = (0usize, 0usize);
    for t in texts {
        for c in t.chars() {
            match bidi_class(c) {
                BidiClass::R | BidiClass::AL => rtl += 1,
                BidiClass::L => ltr += 1,
                _ => {}
            }
        }
    }
    if rtl > ltr {
        Direction::Rtl
    } else {
        Direction::Ltr
    }
}

/// Перевод визуального порядка в логический для одного отрезка текста.
///
/// Перестановка по L2 обратима: применённая к визуальной строке с тем же
/// уровнем абзаца, она возвращает логическую. Поэтому здесь достаточно
/// прогнать стандартный алгоритм — отдельная «обратная» реализация не нужна.
pub(crate) fn visual_to_logical(text: &str, direction: Direction) -> String {
    if text.is_empty() {
        return String::new();
    }
    let info = BidiInfo::new(text, Some(direction.level()));
    let mut out = String::with_capacity(text.len());
    for para in &info.paragraphs {
        out.push_str(&info.reorder_line(para, para.range.clone()));
    }
    reattach_marks(&out)
}

/// Вернуть огласовки и диакритику на их буквы.
///
/// Стандарт переставляет символы, а не кластеры: пара «буква + знак» после
/// разворота становится «знак + буква», и `שָׁלוֹם` рассыпается. Знаки при этом
/// не теряются и не перемешиваются между буквами — достаточно вернуть каждой
/// группе её основу вперёд, сохранив порядок самих знаков.
fn reattach_marks(text: &str) -> String {
    if !text.chars().any(unicode_normalization::char::is_combining_mark) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !unicode_normalization::char::is_combining_mark(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && unicode_normalization::char::is_combining_mark(chars[i]) {
            i += 1;
        }
        match chars.get(i) {
            // Знаки перед буквой — след разворота: основа идёт первой, знаки
            // за ней в обратном порядке, каким они были до перестановки.
            Some(&base) => {
                out.push(base);
                out.extend(chars[start..i].iter().rev());
                i += 1;
            }
            // Знаки в конце строки без основы оставляем как есть.
            None => out.extend(&chars[start..i]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hebrew_document_reorders_line_with_latin_job_title() {
        // Так эта строка лежит в потоке ивритского резюме.
        let visual = "בושחמ יתוריש System Administrator Genie 2013-2017";
        let logical = visual_to_logical(visual, Direction::Rtl);
        assert!(
            logical.contains("שירותי מחשוב"),
            "иврит должен встать в порядок чтения: {logical:?}"
        );
        assert!(
            logical.contains("System Administrator Genie"),
            "латинская должность не должна пострадать: {logical:?}"
        );
    }

    /// Английский документ с ивритской вставкой: порядок строки сохраняется,
    /// разворачивается только само вкрапление.
    #[test]
    fn latin_document_keeps_line_order() {
        let logical = visual_to_logical("Hello לש world", Direction::Ltr);
        assert_eq!(logical, "Hello של world");
    }

    #[test]
    fn direction_follows_the_document_not_the_line() {
        assert_eq!(dominant_direction(["שלום עולם"].into_iter()), Direction::Rtl);
        assert_eq!(dominant_direction(["Hello world"].into_iter()), Direction::Ltr);
        // Одно ивритское слово среди английского текста документ не переворачивает.
        assert_eq!(
            dominant_direction(["Hello של world, a long English sentence"].into_iter()),
            Direction::Ltr
        );
    }


    /// Иврит без presentation-форм: раньше он не проходил переупорядочивание
    /// вовсе и выходил перевёрнутым пословно.
    #[test]
    fn hebrew_reorders_from_visual_order() {
        assert_eq!(visual_to_logical("םלוע םולש", Direction::Rtl), "שלום עולם");
    }

    /// Базовый арабский тоже приходит визуально: отсутствие presentation-форм
    /// говорит лишь о том, что producer записал базовые кодовые точки.
    #[test]
    fn base_arabic_reorders_from_visual_order() {
        assert_eq!(visual_to_logical("\u{627}\u{628}\u{62d}\u{631}\u{645}", Direction::Rtl), "مرحبا");
    }

    /// Не-ASCII латиница внутри ивритской строки.
    #[test]
    fn non_ascii_latin_survives() {
        let logical = visual_to_logical("Fran\u{e7}ois M\u{fc}ller \u{5dd}\u{5d5}\u{5dc}\u{5e9}", Direction::Rtl);
        assert!(logical.contains("François Müller"), "получено: {logical:?}");
        assert!(logical.contains("שלום"), "получено: {logical:?}");
    }

    /// Точная строка из резюме, на которой правка не сработала.
    #[test]
    fn exact_line_from_the_cv() {
        let visual = "\u{5d1}\u{5d5}\u{5e9}\u{5d7}\u{5de} \u{5d9}\u{5ea}\u{5d5}\u{5e8}\u{5d9}\u{5e9} System Administrator Genie-2017 -2013";
        let logical = visual_to_logical(visual, Direction::Rtl);
        println!("  получено: {logical}");
        assert!(logical.contains("שירותי מחשוב"), "получено: {logical:?}");
    }

    #[test]
    fn arabic_indic_digits_keep_their_order() {
        let visual = "השקבב ٠٥٤١٢٣٤٥٦٧ ילש ןופלטה רפסמ";
        let logical = visual_to_logical(visual, Direction::Rtl);
        assert!(logical.contains("٠٥٤١٢٣٤٥٦٧"), "номер не должен вывернуться: {logical:?}");
    }

    #[test]
    fn combining_marks_stay_with_their_letter() {
        let visual = "םלָוֹע םוֹלשָׁ";
        assert_eq!(visual_to_logical(visual, Direction::Rtl), "שָׁלוֹם עוֹלָם");
    }
}
