//! Local language detection at ingest (docs/TRANSLATION.md, "Detection").
//!
//! Pure and free: `whatlang` is a trigram classifier compiled into the binary,
//! so English mail costs one function call and nothing else. The answer is a
//! verdict on the text, not on the headers: `Content-Language` is consulted
//! only when whatlang itself is unsure.

/// The fewest letters a judgement is made on. Below it a trigram model is
/// guessing, and a wrong guess is a model call spent "translating" English.
pub const MIN_LETTERS: usize = 40;

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
/// too little text, or a verdict whatlang will not stand behind.
///
/// Quoted lines (`>`) are dropped first, so an English reply over a foreign
/// quote reads as English. `content_language` is the MIME header, used only as
/// a tiebreaker: when whatlang is unsure, a header that names the language
/// whatlang leaned toward settles it; a header never overrules a reliable
/// verdict and never stands in for one on its own.
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
    if text.chars().filter(|c| c.is_alphabetic()).count() < MIN_LETTERS {
        return None;
    }
    let info = whatlang::detect(&text)?;
    let lang = Lang(info.lang());
    let decided = info.is_reliable()
        || content_language
            .and_then(header_lang)
            .is_some_and(|h| h == lang);
    (decided && !lang.is_english()).then_some(lang)
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

    #[test]
    fn the_header_breaks_a_tie_and_never_overrules() {
        // Short and close between Spanish and Portuguese: whatlang leans but
        // will not stand behind it alone.
        let subject = "Obrigado";
        let body = "Obrigado pela sua mensagem, vamos responder em breve.";
        let info = whatlang::detect(&format!("{subject}\n{body}\n")).unwrap();
        assert!(!info.is_reliable(), "fixture must be a tie: {info:?}");
        let leaned = iso_639_1(info.lang());
        assert_eq!(code(subject, body, None), None);
        assert_eq!(
            code(subject, body, Some(&format!("{leaned}-XX"))),
            Some(leaned)
        );
        // A header naming some other language does not decide it.
        assert_eq!(code(subject, body, Some("de")), None);
        // A reliable English verdict ignores a header that says French.
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
