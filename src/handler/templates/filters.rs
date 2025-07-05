use fluent_templates::LanguageIdentifier;

pub fn t(
    language: &LanguageIdentifier,
    _: &dyn askama::Values,
    text_id: &str,
) -> askama::Result<String> {
    Ok(super::t(language, text_id))
}

// pub fn ta<'a>(
//     language: &'a LanguageIdentifier,
//     text_id: &'a str,
//     args: &'a [(&'a str, impl Into<FluentValue<'a>> + Clone + 'a)],
// ) -> askama::Result<String> {
//     let args = args
//         .iter()
//         .map(|(key, value)| (key.to_string(), value.clone().into()))
//         .collect::<HashMap<_, _>>();
//     Ok(LOCALES
//         .lookup_with_args(language, text_id, &args)
//         .unwrap_or_else(|| format!("ta({})", text_id)))
// }
