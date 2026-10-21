use super::*;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn passes(src: &str, out: &str) -> bool {
    missing_facts(&v(&[src]), &v(&[out])).is_empty()
}

#[test]
fn separators_inside_a_number_are_ignored_across_conventions() {
    assert!(passes("Total : 1.234,56 €", "Total: €1,234.56"));
    assert!(passes("Betrag 1 234,50", "Amount 1,234.50"));
    assert!(passes("Betrag 1\u{202F}234,50", "Amount 1,234.50"));
    // A changed amount is caught.
    assert_eq!(
        missing_facts(&v(&["Total : 1.234,56 €"]), &v(&["Total: €1,234.65"])),
        v(&["123456"])
    );
}

#[test]
fn urls_emails_and_codes_must_survive_verbatim() {
    let src = v(&[
        "Suivez votre colis sur https://track.example.fr/a?id=7, écrivez à aide@example.fr, code ZX81Q9.",
    ]);
    let good = v(&[
        "Track your parcel at https://track.example.fr/a?id=7, write to aide@example.fr, code ZX81Q9.",
    ]);
    assert!(missing_facts(&src, &good).is_empty());
    assert!(added_facts(&src, &good).is_empty());
    let bad = v(&["Track your parcel on the website, write to us, code ZX81Q9."]);
    assert_eq!(
        missing_facts(&src, &bad),
        v(&["https://track.example.fr/a?id=7", "aide@example.fr"])
    );
    let bad = v(&[
        "Track your parcel at https://track.example.fr/a?id=7, write to aide@example.fr, code ZX8IQ9.",
    ]);
    assert_eq!(missing_facts(&src, &bad), v(&["ZX81Q9"]));
}

#[test]
fn an_uppercase_word_is_not_a_code() {
    // FACTURE becomes INVOICE; that is translation, not a lost fact.
    assert!(passes("FACTURE URGENTE", "URGENT INVOICE"));
}

#[test]
fn counts_matter_and_order_does_not() {
    assert!(passes("le 21/10/2026", "on 10/21/2026"));
    assert_eq!(
        missing_facts(
            &v(&["2 adultes et 2 enfants"]),
            &v(&["2 adults and children"])
        ),
        v(&["2"])
    );
    // A number that moved to the neighbouring segment was not lost.
    assert!(
        missing_facts(
            &v(&["Vous avez ", "3 messages"]),
            &v(&["You have 3", "messages"])
        )
        .is_empty()
    );
}

#[test]
fn plain_prose_has_no_facts() {
    assert!(facts("Bonjour, à bientôt !").is_empty());
}

/// The probe cases from the review, each of which used to fail a faithful
/// translation (and buy a retry).
#[test]
fn cjk_text_glued_to_codes_and_urls() {
    assert_eq!(facts("ご注文番号ABC12345です"), v(&["ABC12345"]));
    assert!(passes(
        "ご注文番号ABC12345です",
        "Your order number is ABC12345."
    ));
    assert!(passes(
        "詳細はhttps://example.jp/a/1をご覧ください",
        "See https://example.jp/a/1 for details."
    ));
    assert_eq!(
        facts("詳細：https://example.jp/a/1。よろしく"),
        v(&["https://example.jp/a/1"])
    );
    assert!(passes(
        "詳細：https://example.jp/a/1。よろしく",
        "Details: https://example.jp/a/1. Best regards."
    ));
    assert_eq!(
        facts("「https://example.jp/x」、または"),
        v(&["https://example.jp/x"])
    );
    assert_eq!(facts("連絡先：info@example.jp。"), v(&["info@example.jp"]));
    // An ASCII letter still glues: that is a word, not a code.
    assert!(facts("xABC12345").iter().all(|f| f != "ABC12345"));
}

#[test]
fn fullwidth_and_other_script_digits_are_normalized() {
    assert!(passes("金額は１２３円", "The amount is 123 yen."));
    assert!(passes("المبلغ ٣٤٥ ريال", "The amount is 345 riyals."));
    assert!(passes("مبلغ ۱۲۳۴ تومان", "The amount is 1234 toman."));
    assert!(passes("राशि ९९ रुपये", "Amount 99 rupees."));
    assert!(passes("注文番号ＡＢＣ１２３４５", "Order number ABC12345"));
    assert!(!passes("金額は１２３円", "The amount is 124 yen."));
}

#[test]
fn times_compare_across_separators() {
    assert!(passes("um 14.30 Uhr", "at 14:30"));
    assert!(passes("à 14h30", "at 14:30"));
    assert!(passes("14時30分に", "at 14:30"));
    assert!(passes("14点30分", "at 14:30"));
    assert!(passes("14시 30분에", "at 14:30"));
    assert!(!passes("um 14.30 Uhr", "at 14:40"));
}

#[test]
fn a_space_joins_only_a_thousands_group() {
    assert!(passes("Zimmer 12 2 Personen", "Room 12, 2 people"));
    assert_eq!(
        facts("Zimmer 12 2 Personen"),
        v(&["12", "2"]),
        "two numbers, not 122"
    );
    assert_eq!(facts("1 234 567 €"), v(&["1234567"]));
    assert_eq!(facts("12 3456"), v(&["12", "3456"]));
}

#[test]
fn a_cjk_month_may_become_its_english_name() {
    assert!(passes("2026年10月21日に発送", "Ships on October 21, 2026."));
    assert!(passes("2026年10月21日に発送", "Ships on Oct 21, 2026."));
    assert!(passes("2026年10月21日に発送", "Ships on 2026/10/21."));
    assert!(passes("2026년 3월 5일", "March 5, 2026"));
    // The wrong month, or a lost day, is still caught.
    assert_eq!(
        missing_facts(
            &v(&["2026年10月21日に発送"]),
            &v(&["Ships on November 21, 2026."])
        ),
        v(&["10"])
    );
    assert_eq!(
        missing_facts(&v(&["2026年10月21日"]), &v(&["October 2026"])),
        v(&["21"])
    );
    // Only a month number is lenient: "10 people" needs its 10.
    assert!(!passes("10人", "October people"));
}

#[test]
fn markers_are_not_facts() {
    assert!(facts("⟦1⟧Bonjour⟦/1⟧ ⟦2⟧Marie⟦/2⟧").is_empty());
    assert_eq!(facts("Code ⟦1⟧ZX81Q9⟦/1⟧"), v(&["ZX81Q9"]));
}

#[test]
fn additions_are_caught() {
    let src = v(&["Votre colis arrive demain."]);
    // A phone number is one run, however it is spaced.
    assert_eq!(
        added_facts(
            &src,
            &v(&["Your parcel arrives tomorrow. Call +1 555 0100."])
        ),
        v(&["+1 555 0100"])
    );
    assert_eq!(
        added_facts(
            &src,
            &v(&[
                "Your parcel arrives tomorrow, see https://evil.example/x or mail a@evil.example"
            ])
        ),
        v(&["https://evil.example/x", "a@evil.example"])
    );
    assert_eq!(
        added_facts(&src, &v(&["Your parcel AB12345 arrives tomorrow."])),
        v(&["AB12345"])
    );
    assert_eq!(
        added_facts(
            &src,
            &v(&["Your parcel arrives tomorrow, total 1,250,000."])
        ),
        v(&["1,250,000"])
    );
    // Small numbers are translation: "trois" is 3.
    assert!(check(&v(&["trois colis"]), &v(&["3 parcels"])).passed());
    // A date's year, a CJK month spelled out: nothing new.
    assert!(
        check(
            &v(&["2026年10月21日に発送"]),
            &v(&["Ships on October 21, 2026."])
        )
        .passed()
    );
    assert!(check(&v(&["14時30分に"]), &v(&["at 14:30"])).passed());
    // Repeating a source number is not a new fact.
    assert!(check(&v(&["Total 1234"]), &v(&["Total 1234, again 1234"])).passed());
}

#[test]
fn a_spelled_out_number_is_noted_not_rejected() {
    let c = check(
        &v(&["la somme de deux mille euros"]),
        &v(&["the sum of 2,000 euros"]),
    );
    assert!(c.missing.is_empty() && c.added.is_empty());
    assert_eq!(c.added_numbers, v(&["2,000"]));
    let c = check(&v(&["à dix-huit heures"]), &v(&["at 18:00"]));
    assert!(c.added.is_empty());
    assert_eq!(c.added_numbers, v(&["18:00"]));
}

#[test]
fn magnitudes_derive_their_value_both_ways() {
    for (s, t) in [
        ("料金は3万円です", "The fee is 30,000 yen"),
        ("料金は3万円です", "The fee is 30 thousand yen"),
        ("料金は3万円です", "The fee is 3万 yen"),
        ("1.5万円のご請求", "A charge of 15,000 yen"),
        ("総額1,000万円", "10,000,000 yen in total"),
        ("3万5000円", "35,000 yen"),
        ("총 12만 5천원", "125,000 won in total"),
        ("1,5 Mio. €", "€1.5 million"),
        ("1,5 Mio. €", "€1,500,000"),
        ("Предоплата 15 тыс. руб.", "Prepayment of 15,000 rubles"),
        ("₹2.5 लाख", "₹250,000"),
        ("三千円", "3,000 yen"),
        ("令和8年10月21日", "October 21, 2026"),
    ] {
        let c = check(&v(&[s]), &v(&[t]));
        assert!(c.passed(), "{s:?} -> {t:?}: {c:?}");
    }
    // A wrong value is still a wrong value.
    let c = check(&v(&["料金は3万円です"]), &v(&["The fee is 3,000 yen"]));
    assert_eq!(c.missing, v(&["3"]));
}

#[test]
fn times_and_dates_compare_by_parts() {
    for (s, t) in [
        ("um 18 Uhr", "at 18:00"),
        ("um 18 Uhr", "at 6 PM"),
        ("à 18h", "at 6:00 p.m."),
        ("à 14 h 30", "at 2:30 PM"),
        ("um 14 Uhr 30", "at 14:30"),
        ("午後3時", "3 p.m."),
        ("am 21.10.2026", "on October 21, 2026"),
        ("am 21.10.2026", "on 10/21/2026"),
        ("am 21.10.2026", "on 2026-10-21"),
        ("le 01.10.2026", "on October 1, 2026"),
    ] {
        let c = check(&v(&[s]), &v(&[t]));
        assert!(c.passed(), "{s:?} -> {t:?}: {c:?}");
    }
    assert!(!check(&v(&["um 18 Uhr"]), &v(&["at 6 AM"])).passed());
    assert!(!check(&v(&["am 21.10.2026"]), &v(&["on November 21, 2026"])).passed());
    assert_eq!(facts("am 21.10.2026"), v(&["21", "10", "2026"]));
}

#[test]
fn codes_compare_case_insensitively() {
    assert!(check(&v(&["Réf. abc12345"]), &v(&["Ref. ABC12345"])).passed());
    assert!(check(&v(&["Code ABC12345"]), &v(&["Code abc12345"])).passed());
}

/// The review's 59 faithful translations of everyday idioms (scratchpad
/// probe `facts_q.rs`): a faithful answer must never be REJECTED, and the
/// CJK and German prices, dates and times must all pass outright. Number
/// words (French, Spanish, Roman numerals) are `incomplete` by design.
#[test]
fn faithful_idioms_are_never_rejected() {
    const CASES: &[(&str, &str)] = &[
        ("料金は3万円です", "The fee is 30,000 yen"),
        ("料金は3万円です", "The fee is 30 thousand yen"),
        ("1.5万円のご請求", "A charge of 15,000 yen"),
        ("5千円分のポイント", "5,000 yen worth of points"),
        ("10万円以上のご注文", "Orders of 100,000 yen or more"),
        ("价格为5万元", "The price is 50,000 yuan"),
        ("3만원 할인", "30,000 won discount"),
        ("총 12만 5천원", "125,000 won in total"),
        ("三千円", "3,000 yen"),
        ("一万円", "10,000 yen"),
        ("二〇二六年十月二十一日", "October 21, 2026"),
        ("令和8年10月21日", "October 21, 2026"),
        ("令和8年10月21日", "October 21, Reiwa 8"),
        ("la somme de deux mille euros", "the sum of 2,000 euros"),
        (
            "la somme de deux mille euros",
            "the sum of two thousand euros",
        ),
        (
            "la somme de mille cinq cents euros (1 500 €)",
            "the sum of one thousand five hundred euros (€1,500)",
        ),
        (
            "la somme de mille cinq cents euros (1 500 €)",
            "the sum of 1,500 euros (€1,500)",
        ),
        ("mil quinientos euros", "1,500 euros"),
        ("à dix-huit heures", "at 18:00"),
        ("à dix-huit heures", "at 6 p.m."),
        ("à 14 h 30", "at 14:30"),
        ("à 14 h 30", "at 2:30 p.m."),
        ("à 14h30", "at 14:30"),
        ("à 3 h 30", "at 3:30"),
        ("um 14 Uhr 30", "at 14:30"),
        ("um 18 Uhr", "at 18:00"),
        ("午後3時", "3 p.m."),
        ("15時30分", "15:30"),
        ("下午3点", "3 p.m."),
        ("am 21.10.2026", "on 21.10.2026"),
        ("am 21.10.2026", "on October 21, 2026"),
        ("am 21.10.2026", "on 10/21/2026"),
        ("le 21/10/2026", "on 10/21/2026"),
        ("le 1er octobre 2026", "on October 1st, 2026"),
        ("le 21 octobre 2026", "on October 21, 2026"),
        ("21. Oktober 2026", "October 21, 2026"),
        ("2026年10月21日", "October 21, 2026"),
        ("2026年10月21日", "2026-10-21"),
        ("XXIe siècle", "21st century"),
        ("MMXXVI", "2026"),
        ("le 1er", "the 1st"),
        ("Chambre n° 1204", "Room no. 1204"),
        ("tél. 01 23 45 67 89", "tel. 01 23 45 67 89"),
        ("tél. 01 23 45 67 89", "tel. +33 1 23 45 67 89"),
        ("Réf. abc12345", "Ref. ABC12345"),
        ("Code postal 75001 Paris", "Postal code 75001 Paris"),
        ("1,5 Mio. €", "€1.5 million"),
        ("1,5 Mio. €", "€1,500,000"),
        ("2,5 kg", "2.5 kg"),
        ("50 %", "50%"),
        ("Prix : 1.234,56 €", "Price: €1,234.56"),
        ("Prix : 1 234,56 €", "Price: €1,234.56"),
        ("Предоплата 15 тыс. руб.", "Prepayment of 15,000 rubles"),
        (
            "Предоплата 15 тыс. руб.",
            "Prepayment of 15 thousand rubles",
        ),
        ("₹2.5 लाख", "₹250,000"),
        ("₹2.5 लाख", "₹2.5 lakh"),
        ("Bestellung Nr. 4711", "Order no. 4711"),
        ("seit 1999", "since 1999"),
        ("an mil neuf cent quatre-vingt-dix-neuf", "the year 1999"),
    ];
    assert_eq!(CASES.len(), 59);
    // Exactly the cases a number word or a Roman numeral explains, and
    // nothing in CJK or German.
    const INCOMPLETE: &[&str] = &[
        "la somme de deux mille euros|the sum of 2,000 euros",
        "mil quinientos euros|1,500 euros",
        "à dix-huit heures|at 18:00",
        "à dix-huit heures|at 6 p.m.",
        "MMXXVI|2026",
        "an mil neuf cent quatre-vingt-dix-neuf|the year 1999",
    ];
    let mut incomplete = Vec::new();
    for (s, t) in CASES {
        let c = check(&v(&[s]), &v(&[t]));
        assert!(
            c.added.is_empty(),
            "{s:?} -> {t:?} would be rejected: {c:?}"
        );
        if !c.passed() {
            incomplete.push(format!("{s}|{t}"));
        }
    }
    assert_eq!(incomplete, INCOMPLETE);
}
