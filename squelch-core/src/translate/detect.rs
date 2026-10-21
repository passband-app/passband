//! Local language detection at ingest (docs/TRANSLATION.md, "Detection").
//!
//! Pure and free: `whatlang` is a trigram classifier compiled into the binary,
//! so English mail costs one function call and nothing else. The answer is a
//! verdict on the text, not on the headers: `Content-Language` is consulted
//! only when the text itself leaves English a live possibility.
//!
//! THE QUESTION ASKED IS "IS THIS ENGLISH", NOT "WHICH LANGUAGE IS THIS".
//! whatlang's own `is_reliable()` answers the second one: its confidence is the
//! gap between the top two guesses over every language it knows, so a short
//! Portuguese note scores near zero because Spanish is close behind, and a
//! two-sentence German mail was dropped at 0.61. Personal mail is short, and it
//! is the mail that most needs translating. So the verdict here is the top
//! guess put to a TWO-WAY detection against English alone: German against
//! English is 1.0 on two sentences, while English that merely carries a German
//! place name or a "Merci!" stays under [`MIN_MARGIN_OVER_ENGLISH`].
//!
//! A wrong language NAME costs nothing: the translator reports the language it
//! actually read, and that is what is stored. A wrong "not English" costs one
//! short model call that answers `not_needed`, after which the row is closed.

/// The fewest letters a judgement is made on in a script English is written
/// in. Below it a trigram model is guessing, and a wrong guess is a model call
/// spent "translating" English.
pub const MIN_LETTERS: usize = 40;
/// The floor in any other script (Cyrillic, Han, kana, Arabic, ...). English is
/// not a candidate there at all, and one CJK character is a word, so a whole
/// sentence can be under twenty letters.
pub const MIN_LETTERS_NON_LATIN: usize = 12;
/// How clearly the top guess must beat English, as whatlang's confidence on a
/// detection restricted to the two of them. Foreign mail of a sentence or two
/// scores 1.0; English with foreign names in it scored 0.0 to 0.4 in the
/// fixtures below.
pub const MIN_MARGIN_OVER_ENGLISH: f64 = 0.6;

/// A language detection named, carried as whatlang's own enum so the English
/// name and the ISO codes come from one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lang(whatlang::Lang);

impl Lang {
    /// The ISO 639-1 code stored in `message_translations.source_lang`.
    pub fn code(self) -> &'static str {
        iso_639_1(self.0)
    }

    /// The language's English name, for the reader's "Translated from French".
    pub fn english_name(self) -> &'static str {
        use whatlang::Lang as W;
        // whatlang's own names, where they are not what a reader calls it.
        match self.0 {
            W::Cmn => "Chinese",
            W::Nob => "Norwegian",
            W::Sin => "Sinhala",
            W::Slv => "Slovenian",
            other => other.eng_name(),
        }
    }

    /// The language a stored ISO 639-1 code names, or `None` for a code no
    /// detection would have written.
    pub fn from_code(code: &str) -> Option<Self> {
        let code = code.trim();
        whatlang::Lang::all()
            .iter()
            .copied()
            .find(|l| iso_639_1(*l).eq_ignore_ascii_case(code))
            .map(Lang)
    }

    pub fn is_english(self) -> bool {
        self.0 == whatlang::Lang::Eng
    }
}

/// The language `subject` and `body` are written in, when it is NOT English
/// and the text says so with confidence. `None` is the common answer: English,
/// too little text, or a verdict that leaves English a live possibility.
///
/// Quoted lines (`>`) are dropped first, so an English reply over a foreign
/// quote reads as English. `content_language` is the MIME header, used only as
/// a tiebreaker: when the text leaves English in play, a header naming the
/// language whatlang leaned toward settles it; a header never overrules the
/// text and never stands in for it on its own.
pub fn detect(subject: &str, body: &str, content_language: Option<&str>) -> Option<Lang> {
    let mut text = String::with_capacity(subject.len() + body.len() + 1);
    text.push_str(subject);
    text.push('\n');
    for line in body.lines() {
        if line.trim_start().starts_with('>') {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if letters < MIN_LETTERS_NON_LATIN {
        return None;
    }
    let info = whatlang::detect(&text)?;
    let lang = Lang(info.lang());
    if lang.is_english() {
        return None;
    }
    // Outside the Latin script English is not a candidate: whatlang picks the
    // script first and only ranks the languages written in it.
    if info.script() != whatlang::Script::Latin {
        return Some(lang);
    }
    if letters < MIN_LETTERS {
        return None;
    }
    let margin = whatlang::Detector::with_allowlist(vec![info.lang(), whatlang::Lang::Eng])
        .detect(&text)
        .filter(|two| two.lang() == info.lang())
        .map_or(0.0, |two| two.confidence());
    let decided = margin >= MIN_MARGIN_OVER_ENGLISH
        || content_language
            .and_then(header_lang)
            .is_some_and(|h| h == lang);
    decided.then_some(lang)
}

/// The primary subtag of the first language a `Content-Language` header
/// names (`fr-CA, en` is French).
fn header_lang(raw: &str) -> Option<Lang> {
    let first = raw.split(',').next()?.trim();
    let primary = first.split(['-', '_']).next()?.trim();
    if primary.len() != 2 {
        return None;
    }
    Lang::from_code(primary)
}

/// ISO 639-1 for every language whatlang knows. All 69 have one.
fn iso_639_1(lang: whatlang::Lang) -> &'static str {
    use whatlang::Lang::*;
    match lang {
        Epo => "eo",
        Eng => "en",
        Rus => "ru",
        Cmn => "zh",
        Spa => "es",
        Por => "pt",
        Ita => "it",
        Ben => "bn",
        Fra => "fr",
        Deu => "de",
        Ukr => "uk",
        Kat => "ka",
        Ara => "ar",
        Hin => "hi",
        Jpn => "ja",
        Heb => "he",
        Yid => "yi",
        Pol => "pl",
        Amh => "am",
        Jav => "jv",
        Kor => "ko",
        Nob => "nb",
        Dan => "da",
        Swe => "sv",
        Fin => "fi",
        Tur => "tr",
        Nld => "nl",
        Hun => "hu",
        Ces => "cs",
        Ell => "el",
        Bul => "bg",
        Bel => "be",
        Mar => "mr",
        Kan => "kn",
        Ron => "ro",
        Slv => "sl",
        Hrv => "hr",
        Srp => "sr",
        Mkd => "mk",
        Lit => "lt",
        Lav => "lv",
        Est => "et",
        Tam => "ta",
        Vie => "vi",
        Urd => "ur",
        Tha => "th",
        Guj => "gu",
        Uzb => "uz",
        Pan => "pa",
        Aze => "az",
        Ind => "id",
        Tel => "te",
        Pes => "fa",
        Mal => "ml",
        Ori => "or",
        Mya => "my",
        Nep => "ne",
        Sin => "si",
        Khm => "km",
        Tuk => "tk",
        Aka => "ak",
        Zul => "zu",
        Sna => "sn",
        Afr => "af",
        Lat => "la",
        Slk => "sk",
        Cat => "ca",
        Tgl => "tl",
        Hye => "hy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(subject: &str, body: &str, header: Option<&str>) -> Option<&'static str> {
        detect(subject, body, header).map(Lang::code)
    }

    #[test]
    fn english_is_none_and_foreign_is_named() {
        assert_eq!(
            code(
                "Your order has shipped",
                "Hi there, your package left our warehouse this morning and should arrive on Thursday.",
                None
            ),
            None
        );
        assert_eq!(
            code(
                "Votre commande a été expédiée",
                "Bonjour, votre colis a quitté notre entrepôt ce matin et devrait arriver jeudi prochain.",
                None
            ),
            Some("fr")
        );
        assert_eq!(
            code(
                "Ihre Bestellung wurde versandt",
                "Guten Tag, Ihr Paket hat heute Morgen unser Lager verlassen und sollte am Donnerstag bei Ihnen ankommen. Bei Fragen wenden Sie sich bitte an unseren Kundenservice.",
                None
            ),
            Some("de")
        );
        assert_eq!(
            code(
                "ご注文の商品を発送しました",
                "いつもご利用いただきありがとうございます。ご注文の商品は本日倉庫から発送されました。木曜日に到着する予定です。",
                None
            ),
            Some("ja")
        );
        assert_eq!(
            code(
                "Su pedido ha sido enviado",
                "Hola, su paquete salió de nuestro almacén esta mañana y debería llegar el jueves por la tarde.",
                None
            ),
            Some("es")
        );
    }

    #[test]
    fn too_little_text_is_none() {
        assert_eq!(code("Merci", "À demain !", None), None);
        assert_eq!(code("Merci", "À demain !", Some("fr")), None);
        assert_eq!(code("Re: Termin", "Danke, passt! Viele Grüße", None), None);
        assert_eq!(code("谢谢", "好的", None), None);
    }

    #[test]
    fn an_english_reply_over_a_foreign_quote_is_english() {
        let body = "Thanks so much, that works for me and I will see you on Thursday at the office.\n\
                    \n\
                    > Bonjour, je vous confirme notre rendez-vous de jeudi prochain au bureau.\n\
                    > Pourriez-vous apporter les documents signés et la facture du mois dernier ?\n\
                    > Merci beaucoup et à bientôt, cordialement.";
        assert_eq!(code("Re: Rendez-vous", body, None), None);
    }

    /// THE SHORT PERSONAL MAIL `is_reliable()` dropped: one or two sentences,
    /// which is what a person writes. Several of these score under 0.2 on
    /// whatlang's own confidence (a neighbouring language is close behind)
    /// and every one of them beats English outright. Where whatlang names the
    /// wrong neighbour (Spanish read as Portuguese) the translator's answer
    /// replaces it; what matters here is that none of them is called English.
    #[test]
    fn short_personal_mail_is_detected() {
        let cases = [
            (
                "Treffen morgen",
                "Hallo Anna, können wir uns morgen um zehn Uhr im Büro treffen? Ich bringe die Unterlagen mit.",
                "de",
            ),
            (
                "Dîner samedi",
                "Salut Marc, tu es libre samedi soir pour dîner chez nous ? Dis-moi vite.",
                "fr",
            ),
            (
                "Cena",
                "Hola Juan, ¿quedamos el viernes para cenar? Avísame cuando puedas.",
                "es",
            ),
            (
                "Jantar",
                "Oi Ana, vamos jantar na sexta? Me avisa quando puder, beijos.",
                "pt",
            ),
            (
                "Cena venerdì",
                "Ciao Marco, ci vediamo venerdì sera per cena? Fammi sapere presto.",
                "it",
            ),
            (
                "明日の会議",
                "明日の会議は十時からです。資料を持ってきてください。",
                "ja",
            ),
            ("明天开会", "明天上午十点开会，请带上文件。谢谢！", "zh"),
            (
                "Встреча",
                "Привет, давай встретимся завтра в десять утра в офисе. Принеси документы.",
                "ru",
            ),
            (
                "Rendez-vous",
                "Bonjour Madame, je confirme notre rendez-vous de mardi à 14h. Cordialement, Pierre",
                "fr",
            ),
            (
                "Bedankt",
                "Hoi, bedankt voor je bericht, ik bel je morgen terug.",
                "nl",
            ),
        ];
        for (subject, body, want) in cases {
            assert_eq!(code(subject, body, None), Some(want), "{subject}: {body}");
        }
        // Detected as not English, under a neighbour's name.
        assert!(
            code(
                "Saludos",
                "Gracias por todo, nos vemos pronto. Un abrazo fuerte.",
                None
            )
            .is_some()
        );
    }

    /// English that a looser rule could mistake: short notes, foreign names
    /// and places, a foreign sign-off, a wine newsletter full of French and
    /// Italian proper nouns, a receipt with a German company in it.
    #[test]
    fn english_with_foreign_words_is_not_detected() {
        let cases = [
            (
                "Lunch tomorrow?",
                "Hey, are you free for lunch tomorrow around noon? Let me know.",
            ),
            (
                "Quick question",
                "Can you send me the report by Friday? Thanks a lot.",
            ),
            ("Re: meeting", "Sounds good, see you then. Merci!"),
            (
                "Intro",
                "Hi Jean-Pierre, meet François Dubois from our Lyon office. He will help.",
            ),
            (
                "Thanks",
                "Merci beaucoup for the help yesterday, see you soon. À bientôt!",
            ),
            (
                "Trip",
                "We land in München on Tuesday and drive to Garmisch-Partenkirchen after.",
            ),
            (
                "Hola",
                "Hola amigo! Long time no see, how are things in Madrid these days?",
            ),
            (
                "Ciao",
                "Ciao bella, grazie mille for the photos, they are wonderful.",
            ),
            (
                "Order",
                "Your order from Zalando GmbH has shipped. Tracking: 1Z999AA10123456784.",
            ),
            (
                "Your receipt",
                "Thanks for your order. Total: $45.00. Order #12345. Questions? Reply to this email.",
            ),
            (
                "Newsletter",
                "Discover our new collection: Château Margaux, Prosecco di Valdobbiadene, and more wines from Bordeaux and Toscana.",
            ),
            (
                "Signoff",
                "Thanks for the update, I will review the contract tonight.\n\nGrüße,\nJürgen Müller",
            ),
            (
                "Weekly digest",
                "This week in tech: Apple announced new chips, Google updated search, and startups raised record funding across the board.",
            ),
            ("FYI", "See attached. Best, Hiroshi Tanaka (田中 博)"),
            ("Hi", "Ok see you at 5"),
        ];
        for (subject, body) in cases {
            assert_eq!(code(subject, body, None), None, "{subject}: {body}");
        }
    }

    #[test]
    fn the_header_breaks_a_tie_and_never_overrules() {
        // English with German place names: whatlang leans German, but not
        // clearly enough over English to stand behind it alone.
        let subject = "Trip";
        let body = "We land in München on Tuesday and drive to Garmisch-Partenkirchen after.";
        let text = format!("{subject}\n{body}\n");
        let lean = whatlang::detect(&text).unwrap().lang();
        assert_eq!(lean, whatlang::Lang::Deu, "fixture must lean German");
        let margin = whatlang::Detector::with_allowlist(vec![lean, whatlang::Lang::Eng])
            .detect(&text)
            .unwrap()
            .confidence();
        assert!(
            margin < MIN_MARGIN_OVER_ENGLISH,
            "fixture must be a tie: {margin}"
        );
        assert_eq!(code(subject, body, None), None);
        assert_eq!(code(subject, body, Some("de-AT")), Some("de"));
        // A header naming some other language does not decide it.
        assert_eq!(code(subject, body, Some("fr")), None);
        // An English verdict ignores a header that says French.
        assert_eq!(
            code(
                "Your order has shipped",
                "Hi there, your package left our warehouse this morning and should arrive on Thursday.",
                Some("fr")
            ),
            None
        );
    }

    #[test]
    fn codes_round_trip_and_names_read_naturally() {
        for l in whatlang::Lang::all() {
            let lang = Lang(*l);
            assert_eq!(lang.code().len(), 2);
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("fr").unwrap().english_name(), "French");
        assert_eq!(Lang::from_code("zh").unwrap().english_name(), "Chinese");
        assert_eq!(Lang::from_code("xx"), None);
    }
}
