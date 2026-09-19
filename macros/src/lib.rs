//! Derive macros for `typesafeai-sdk-community`.
//!
//! Use them through the main crate (`typesafeai_sdk_community::{ChoiceLabels, ScoreLevels, Questions}`)
//! rather than depending on this crate directly. See the main crate's documentation for the
//! attribute reference and examples.

use heck::{ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutySnakeCase, ToSnakeCase};
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, Data, DeriveInput, Expr, ExprLit, Fields, Ident, Lit, LitStr, Token, Type};

// --- Attribute argument parsing ---------------------------------------------------------------

/// One argument inside `#[choice(...)]`, `#[score(...)]`, `#[noul(...)]`, or `#[questions(...)]`:
/// either `key = expr` or a bare positional expression.
enum Arg {
    Named { key: String, span: Span, value: Expr },
    Positional(Expr),
}

impl Parse for Arg {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(Token![crate]) && input.peek2(Token![=]) {
            let key: Token![crate] = input.parse()?;
            input.parse::<Token![=]>()?;
            return Ok(Arg::Named { key: "crate".into(), span: key.span, value: input.parse()? });
        }
        if input.peek(Ident) && input.peek2(Token![=]) {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            return Ok(Arg::Named { key: key.to_string(), span: key.span(), value: input.parse()? });
        }
        Ok(Arg::Positional(input.parse()?))
    }
}

struct Args {
    positional: Vec<Expr>,
    named: Vec<(String, Span, Expr)>,
}

impl Args {
    fn parse_attrs(attrs: &[Attribute], name: &str) -> syn::Result<Option<Args>> {
        let mut found = None::<Args>;
        for attr in attrs.iter().filter(|attr| attr.path().is_ident(name)) {
            let args = match &attr.meta {
                syn::Meta::Path(_) => Vec::new(),
                _ => attr.parse_args_with(Punctuated::<Arg, Token![,]>::parse_terminated)?.into_iter().collect(),
            };
            let target = found.get_or_insert_with(|| Args { positional: Vec::new(), named: Vec::new() });
            for arg in args {
                match arg {
                    Arg::Named { key, span, value } => target.named.push((key, span, value)),
                    Arg::Positional(expr) => target.positional.push(expr),
                }
            }
        }
        Ok(found)
    }

    fn take(&mut self, key: &str) -> Option<Expr> {
        let index = self.named.iter().position(|(k, _, _)| k == key)?;
        Some(self.named.remove(index).2)
    }

    fn take_str(&mut self, key: &str) -> syn::Result<Option<String>> {
        match self.take(key) {
            None => Ok(None),
            Some(expr) => string_literal(&expr).map(Some),
        }
    }

    /// The first positional argument, if any. Extras are an error.
    fn positional(&mut self, what: &str) -> syn::Result<Option<Expr>> {
        if self.positional.len() > 1 {
            return Err(syn::Error::new_spanned(&self.positional[1], format!("only one positional {what} is allowed")));
        }
        Ok(self.positional.pop())
    }

    fn finish(self, allowed: &[&str]) -> syn::Result<()> {
        if let Some((key, span, _)) = self.named.first() {
            return Err(syn::Error::new(
                *span,
                format!("unknown attribute `{key}`; expected one of: {}", allowed.join(", ")),
            ));
        }
        if let Some(extra) = self.positional.first() {
            return Err(syn::Error::new_spanned(extra, "unexpected positional argument"));
        }
        Ok(())
    }
}

fn string_literal(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Str(lit), .. }) => Ok(lit.value()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

/// Resolve `crate = "..."` to a path, defaulting to the published crate name.
fn crate_path(args: &mut Option<Args>) -> syn::Result<TokenStream2> {
    let Some(args) = args.as_mut() else { return Ok(quote!(::typesafeai_sdk_community)) };
    match args.take("crate") {
        None => Ok(quote!(::typesafeai_sdk_community)),
        Some(expr) => {
            let path: syn::Path = LitStr::new(&string_literal(&expr)?, expr.span()).parse()?;
            Ok(path.to_token_stream())
        }
    }
}

trait SpanOf {
    fn span(&self) -> Span;
}
impl SpanOf for Expr {
    fn span(&self) -> Span {
        syn::spanned::Spanned::span(self)
    }
}

fn rename(variant: &str, rule: &str, span: Span) -> syn::Result<String> {
    Ok(match rule {
        "snake_case" => variant.to_snake_case(),
        "lowercase" => variant.to_lowercase(),
        "UPPERCASE" => variant.to_uppercase(),
        "kebab-case" => variant.to_kebab_case(),
        "camelCase" => variant.to_lower_camel_case(),
        "PascalCase" => variant.to_pascal_case(),
        "SCREAMING_SNAKE_CASE" => variant.to_shouty_snake_case(),
        "none" => variant.to_string(),
        other => {
            return Err(syn::Error::new(
                span,
                format!(
                    "unknown rename_all rule {other:?}; expected snake_case, lowercase, UPPERCASE, kebab-case, camelCase, PascalCase, SCREAMING_SNAKE_CASE, or none"
                ),
            ));
        }
    })
}

/// `NeedsAttentionToday` -> `Needs attention today`.
fn humanize(variant: &str) -> String {
    let mut words = variant.to_snake_case().replace('_', " ");
    if let Some(first) = words.get(..1) {
        let upper = first.to_uppercase();
        words.replace_range(..1, &upper);
    }
    words
}

fn unit_variants<'a>(input: &'a DeriveInput, derive: &str) -> syn::Result<Vec<&'a syn::Variant>> {
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, format!("#[derive({derive})] only supports enums")));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(&input.ident, format!("#[derive({derive})] needs at least one variant")));
    }
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                format!("#[derive({derive})] only supports unit variants (no fields)"),
            ));
        }
    }
    Ok(data.variants.iter().collect())
}

// --- ChoiceLabels -------------------------------------------------------------------------------

/// Derive `ChoiceLabels` for a unit-only enum so its variants are the labels of a choice question.
///
/// Enum attributes: `#[choice(rename_all = "snake_case")]` (default), `#[choice(crate = "path")]`.
/// Variant attributes: `#[choice(label = "angry")]`, `#[choice(describe = "An upset message")]`.
#[proc_macro_derive(ChoiceLabels, attributes(choice))]
pub fn derive_choice_labels(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_choice_labels(&input).unwrap_or_else(|error| error.to_compile_error()).into()
}

fn expand_choice_labels(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let variants = unit_variants(input, "ChoiceLabels")?;
    let mut container = Args::parse_attrs(&input.attrs, "choice")?;
    let krate = crate_path(&mut container)?;
    let mut rule = "snake_case".to_string();
    if let Some(args) = container.as_mut()
        && let Some(value) = args.take("rename_all")
    {
        rule = string_literal(&value)?;
        rename("Probe", &rule, value.span())?;
    }
    if let Some(args) = container {
        args.finish(&["rename_all", "crate"])?;
    }

    let enum_name = &input.ident;
    let mut idents = Vec::new();
    let mut labels = Vec::new();
    let mut describes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for variant in variants {
        let mut args = Args::parse_attrs(&variant.attrs, "choice")?;
        let mut label = rename(&variant.ident.to_string(), &rule, variant.ident.span())?;
        let mut describe = quote!(::core::option::Option::None);
        if let Some(args) = args.as_mut() {
            if let Some(value) = args.take_str("label")? {
                label = value;
            }
            if let Some(expr) = args.take("describe") {
                describe = quote!(::core::option::Option::Some(#krate::Value::from(#expr)));
            }
            if let Some(expr) = args.positional("description")? {
                describe = quote!(::core::option::Option::Some(#krate::Value::from(#expr)));
            }
        }
        if let Some(args) = args {
            args.finish(&["label", "describe"])?;
        }
        if !seen.insert(label.clone()) {
            return Err(syn::Error::new_spanned(&variant.ident, format!("duplicate choice label {label:?}")));
        }
        idents.push(&variant.ident);
        labels.push(label);
        describes.push(describe);
    }

    Ok(quote! {
        impl #krate::typed::ChoiceLabels for #enum_name {
            const ALL: &'static [Self] = &[#(#enum_name::#idents),*];

            fn label(self) -> &'static str {
                match self { #(#enum_name::#idents => #labels),* }
            }

            fn describe(self) -> ::core::option::Option<#krate::Value> {
                match self { #(#enum_name::#idents => #describes),* }
            }

            fn from_label(label: &str) -> ::core::option::Option<Self> {
                match label {
                    #(#labels => ::core::option::Option::Some(#enum_name::#idents),)*
                    _ => ::core::option::Option::None,
                }
            }
        }

        impl #krate::typed::FromAnswer for #enum_name {
            fn from_answer(
                name: &str,
                answer: ::core::option::Option<&#krate::Answer>,
            ) -> ::core::result::Result<Self, #krate::AnswerError> {
                let typed = <#krate::typed::TypedChoice<Self> as #krate::typed::FromAnswer>::from_answer(name, answer)?;
                ::core::result::Result::Ok(typed.choice)
            }
        }

        impl #krate::typed::ChoiceTarget for #enum_name {}

        impl #krate::typed::ChoiceCriteria for #enum_name {
            fn criteria() -> ::std::collections::BTreeMap<::std::string::String, ::core::option::Option<#krate::Value>> {
                <#krate::typed::TypedChoice<Self> as #krate::typed::ChoiceCriteria>::criteria()
            }
        }
    })
}

// --- ScoreLevels --------------------------------------------------------------------------------

/// Derive `ScoreLevels` for a unit-only enum whose declaration order is the rubric, scored from 0.
///
/// Enum attributes: `#[score(crate = "path")]`.
/// Variant attributes: `#[score("Can wait")]` or `#[score(describe = "Can wait")]`; without one,
/// the variant name is humanized (`NeedsAttentionToday` -> `Needs attention today`).
#[proc_macro_derive(ScoreLevels, attributes(score))]
pub fn derive_score_levels(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_score_levels(&input).unwrap_or_else(|error| error.to_compile_error()).into()
}

fn expand_score_levels(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let variants = unit_variants(input, "ScoreLevels")?;
    let mut container = Args::parse_attrs(&input.attrs, "score")?;
    let krate = crate_path(&mut container)?;
    if let Some(args) = container {
        args.finish(&["crate"])?;
    }

    let enum_name = &input.ident;
    let mut idents = Vec::new();
    let mut levels = Vec::new();
    let mut describes = Vec::new();
    for (level, variant) in variants.into_iter().enumerate() {
        let mut args = Args::parse_attrs(&variant.attrs, "score")?;
        let fallback = humanize(&variant.ident.to_string());
        let mut describe = quote!(#krate::Value::from(#fallback));
        if let Some(args) = args.as_mut() {
            if let Some(expr) = args.take("describe") {
                describe = quote!(#krate::Value::from(#expr));
            }
            if let Some(expr) = args.positional("description")? {
                describe = quote!(#krate::Value::from(#expr));
            }
        }
        if let Some(args) = args {
            args.finish(&["describe"])?;
        }
        idents.push(&variant.ident);
        levels.push(level as u32);
        describes.push(describe);
    }

    Ok(quote! {
        impl #krate::typed::ScoreLevels for #enum_name {
            const ALL: &'static [Self] = &[#(#enum_name::#idents),*];

            fn level(self) -> u32 {
                match self { #(#enum_name::#idents => #levels),* }
            }

            fn describe(self) -> #krate::Value {
                match self { #(#enum_name::#idents => #describes),* }
            }

            fn from_level(level: u32) -> ::core::option::Option<Self> {
                match level {
                    #(#levels => ::core::option::Option::Some(#enum_name::#idents),)*
                    _ => ::core::option::Option::None,
                }
            }
        }

        impl #krate::typed::FromAnswer for #enum_name {
            fn from_answer(
                name: &str,
                answer: ::core::option::Option<&#krate::Answer>,
            ) -> ::core::result::Result<Self, #krate::AnswerError> {
                let typed = <#krate::typed::TypedScore<Self> as #krate::typed::FromAnswer>::from_answer(name, answer)?;
                ::core::result::Result::Ok(typed.most_likely)
            }
        }

        impl #krate::typed::ScoreTarget for #enum_name {}

        impl #krate::typed::ScoreCriteria for #enum_name {
            fn criteria() -> ::std::vec::Vec<#krate::Value> {
                <#krate::typed::TypedScore<Self> as #krate::typed::ScoreCriteria>::criteria()
            }
        }
    })
}

// --- Questions ----------------------------------------------------------------------------------

/// Derive `Questions` for a struct whose fields are the questions of one System One request.
///
/// Each field carries exactly one of `#[noul(...)]`, `#[choice(...)]`, or `#[score(...)]`. The
/// first positional argument is the instructions; `name = "..."` overrides the wire name (the
/// field name by default). Nouls accept `when_true = ...` and `when_false = ...`; untyped choices
/// take `labels = ["a", ("b", "description")]`; untyped scores take `levels = ["low", "high"]`.
/// Typed fields (`TypedChoice<T>`, `T: ChoiceLabels`, `TypedScore<T>`, `T: ScoreLevels`) derive
/// their criteria from the type. Wrap any field type in `Option` to tolerate a missing answer.
#[proc_macro_derive(Questions, attributes(questions, noul, choice, score))]
pub fn derive_questions(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_questions(&input).unwrap_or_else(|error| error.to_compile_error()).into()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Noul,
    Choice,
    Score,
}

impl Kind {
    fn attr(self) -> &'static str {
        match self {
            Kind::Noul => "noul",
            Kind::Choice => "choice",
            Kind::Score => "score",
        }
    }
}

fn expand_questions(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "#[derive(Questions)] only supports structs"));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Questions)] only supports structs with named fields",
        ));
    };
    let mut container = Args::parse_attrs(&input.attrs, "questions")?;
    let krate = crate_path(&mut container)?;
    if let Some(args) = container {
        args.finish(&["crate"])?;
    }

    let struct_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mut inserts = Vec::new();
    let mut builds = Vec::new();
    let mut asserts = Vec::new();
    let mut names = std::collections::HashSet::new();

    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named field");
        let ty: &Type = &field.ty;
        let present: Vec<Kind> = [Kind::Noul, Kind::Choice, Kind::Score]
            .into_iter()
            .filter(|kind| field.attrs.iter().any(|attr| attr.path().is_ident(kind.attr())))
            .collect();
        let kind = match present.as_slice() {
            [kind] => *kind,
            [] => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "every field needs exactly one of #[noul(...)], #[choice(...)], or #[score(...)]",
                ));
            }
            _ => return Err(syn::Error::new_spanned(ident, "a field can carry only one question attribute")),
        };
        let mut args = Args::parse_attrs(&field.attrs, kind.attr())?.expect("attribute present");
        let name = args.take_str("name")?.unwrap_or_else(|| ident.to_string());
        if !names.insert(name.clone()) {
            return Err(syn::Error::new_spanned(ident, format!("duplicate question name {name:?}")));
        }
        let instructions = match args.take("instructions") {
            Some(expr) => Some(expr),
            None => args.positional("instructions")?,
        };
        let instructions = match instructions {
            Some(expr) => quote!(::core::option::Option::Some(#krate::Value::from(#expr))),
            None => quote!(::core::option::Option::None),
        };

        let question = match kind {
            Kind::Noul => {
                let mut criteria = quote!(::core::option::Option::None);
                let when_true = args.take("when_true");
                let when_false = args.take("when_false");
                if when_true.is_some() || when_false.is_some() {
                    let t = opt_value(&krate, when_true);
                    let f = opt_value(&krate, when_false);
                    criteria =
                        quote!(::core::option::Option::Some(#krate::NoulCriteria { when_true: #t, when_false: #f }));
                }
                args.finish(&["instructions", "name", "when_true", "when_false"])?;
                asserts.push(quote!(let _ = assert_noul::<#ty>;));
                quote!(#krate::Question::Noul(#krate::Noul { instructions: #instructions, criteria: #criteria }))
            }
            Kind::Choice => {
                let criteria = match args.take("labels") {
                    Some(expr) => labels_expr(&krate, &expr)?,
                    None => quote!(<#ty as #krate::typed::ChoiceCriteria>::criteria()),
                };
                args.finish(&["instructions", "name", "labels"])?;
                asserts.push(quote!(let _ = assert_choice::<#ty>;));
                quote!(#krate::Question::Choice(#krate::Choice { instructions: #instructions, criteria: #criteria }))
            }
            Kind::Score => {
                let criteria = match args.take("levels") {
                    Some(expr) => levels_expr(&krate, &expr)?,
                    None => quote!(<#ty as #krate::typed::ScoreCriteria>::criteria()),
                };
                args.finish(&["instructions", "name", "levels"])?;
                asserts.push(quote!(let _ = assert_score::<#ty>;));
                quote!(#krate::Question::Score(#krate::Score { instructions: #instructions, criteria: #criteria }))
            }
        };
        inserts.push(quote!(map.insert(::std::string::String::from(#name), #question);));
        builds.push(quote! {
            #ident: <#ty as #krate::typed::FromAnswer>::from_answer(#name, response.answer(#name))?
        });
    }

    Ok(quote! {
        impl #impl_generics #krate::typed::Questions for #struct_name #ty_generics #where_clause {
            fn questions() -> ::std::collections::BTreeMap<::std::string::String, #krate::Question> {
                let mut map = ::std::collections::BTreeMap::new();
                #(#inserts)*
                map
            }

            fn from_response(
                response: &#krate::SystemOneResponse,
            ) -> ::core::result::Result<Self, #krate::AnswerError> {
                ::core::result::Result::Ok(Self { #(#builds),* })
            }
        }

        // Kind checks: referencing the generic fn with an explicit type argument fails to compile
        // when the field type does not implement the marker trait for its question kind.
        const _: () = {
            #[allow(dead_code)]
            fn assert_noul<T: #krate::typed::NoulTarget>() {}
            #[allow(dead_code)]
            fn assert_choice<T: #krate::typed::ChoiceTarget>() {}
            #[allow(dead_code)]
            fn assert_score<T: #krate::typed::ScoreTarget>() {}
            #(#asserts)*
        };
    })
}

fn opt_value(krate: &TokenStream2, expr: Option<Expr>) -> TokenStream2 {
    match expr {
        Some(expr) => quote!(::core::option::Option::Some(#krate::Value::from(#expr))),
        None => quote!(::core::option::Option::None),
    }
}

/// `labels = ["a", ("b", "description")]` -> a criteria map expression.
fn labels_expr(krate: &TokenStream2, expr: &Expr) -> syn::Result<TokenStream2> {
    let Expr::Array(array) = expr else {
        return Err(syn::Error::new_spanned(expr, "`labels` must be an array like [\"a\", (\"b\", \"description\")]"));
    };
    let entries = array.elems.iter().map(|elem| match elem {
        Expr::Tuple(tuple) if tuple.elems.len() == 2 => {
            let label = &tuple.elems[0];
            let describe = &tuple.elems[1];
            quote!((::std::string::ToString::to_string(&#label), ::core::option::Option::Some(#krate::Value::from(#describe))))
        }
        other => quote!((::std::string::ToString::to_string(&#other), ::core::option::Option::None)),
    });
    Ok(quote!(::std::collections::BTreeMap::from([#(#entries),*])))
}

/// `levels = ["low", "high"]` -> a criteria vector expression.
fn levels_expr(krate: &TokenStream2, expr: &Expr) -> syn::Result<TokenStream2> {
    let Expr::Array(array) = expr else {
        return Err(syn::Error::new_spanned(expr, "`levels` must be an array like [\"low\", \"high\"]"));
    };
    let entries = array.elems.iter().map(|elem| quote!(#krate::Value::from(#elem)));
    Ok(quote!(::std::vec![#(#entries),*]))
}
