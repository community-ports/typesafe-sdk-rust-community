//! Derive macros for `typesafeai-sdk-community`.
//!
//! Use them through the main crate (`typesafeai_sdk_community::{ChoiceLabels, ScoreLevels, Questions,
//! Route, Composite}`) rather than depending on this crate directly. See the main crate's documentation for the
//! attribute reference and examples.

use heck::{ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutySnakeCase, ToSnakeCase};
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, format_ident, quote};
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

    /// Remove a bare flag such as `mock` or `invert` from the positional arguments.
    fn take_flag(&mut self, flag: &str) -> bool {
        let index = self.positional.iter().position(|expr| match expr {
            Expr::Path(path) => path.path.is_ident(flag),
            _ => false,
        });
        match index {
            Some(index) => {
                self.positional.remove(index);
                true
            }
            None => false,
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

        impl #krate::typed::ChoiceOf for #enum_name {
            type Labels = #enum_name;
        }

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

        impl #krate::typed::ScoreOf for #enum_name {
            type Levels = #enum_name;
        }

        impl #krate::typed::ScoreCriteria for #enum_name {
            fn criteria() -> ::std::vec::Vec<#krate::Value> {
                <#krate::typed::TypedScore<Self> as #krate::typed::ScoreCriteria>::criteria()
            }
        }
    })
}

// --- Shared question-field handling -------------------------------------------------------------

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

/// How a mock fixture builder sets a field's answer.
enum MockShape {
    Noul,
    ChoiceTyped,
    ChoiceLabels,
    ScoreTyped,
    ScoreLevels(usize),
}

/// One question field, parsed from its attribute, with the code it contributes.
struct QuestionField {
    ident: Ident,
    ty: Type,
    name: String,
    question: TokenStream2,
    assert: TokenStream2,
    mock: MockShape,
}

impl QuestionField {
    /// `ident: <ty as FromAnswer>::from_answer(name, response.answer(name))?`
    fn build(&self, krate: &TokenStream2) -> TokenStream2 {
        let (ident, ty, name) = (&self.ident, &self.ty, &self.name);
        quote!(#ident: <#ty as #krate::typed::FromAnswer>::from_answer(#name, response.answer(#name))?)
    }

    fn insert(&self, krate: &TokenStream2) -> TokenStream2 {
        let (name, question) = (&self.name, &self.question);
        let _ = krate;
        quote!(map.insert(::std::string::String::from(#name), #question);)
    }
}

/// Parse a field's `#[noul]`/`#[choice]`/`#[score]` attribute. `prefix` is prepended to the
/// default wire name (route branches use `"<label>."`); an explicit `name = "..."` is used as-is.
fn question_field(
    field: &syn::Field,
    krate: &TokenStream2,
    prefix: &str,
    names: &mut std::collections::HashSet<String>,
) -> syn::Result<QuestionField> {
    let ident = field.ident.clone().expect("named field");
    let ty = field.ty.clone();
    let present: Vec<Kind> = [Kind::Noul, Kind::Choice, Kind::Score]
        .into_iter()
        .filter(|kind| field.attrs.iter().any(|attr| attr.path().is_ident(kind.attr())))
        .collect();
    let kind = match present.as_slice() {
        [kind] => *kind,
        [] => {
            return Err(syn::Error::new_spanned(
                &ident,
                "every field needs exactly one of #[noul(...)], #[choice(...)], or #[score(...)]",
            ));
        }
        _ => return Err(syn::Error::new_spanned(&ident, "a field can carry only one question attribute")),
    };
    let mut args = Args::parse_attrs(&field.attrs, kind.attr())?.expect("attribute present");
    let name = args.take_str("name")?.unwrap_or_else(|| format!("{prefix}{ident}"));
    if !names.insert(name.clone()) {
        return Err(syn::Error::new_spanned(&ident, format!("duplicate question name {name:?}")));
    }
    let instructions = match args.take("instructions") {
        Some(expr) => Some(expr),
        None => args.positional("instructions")?,
    };
    let instructions = match instructions {
        Some(expr) => quote!(::core::option::Option::Some(#krate::Value::from(#expr))),
        None => quote!(::core::option::Option::None),
    };

    let (question, assert, mock) = match kind {
        Kind::Noul => {
            let mut criteria = quote!(::core::option::Option::None);
            let when_true = args.take("when_true");
            let when_false = args.take("when_false");
            if when_true.is_some() || when_false.is_some() {
                let t = opt_value(krate, when_true);
                let f = opt_value(krate, when_false);
                criteria = quote!(::core::option::Option::Some(#krate::NoulCriteria { when_true: #t, when_false: #f }));
            }
            args.finish(&["instructions", "name", "when_true", "when_false"])?;
            (
                quote!(#krate::Question::Noul(#krate::Noul { instructions: #instructions, criteria: #criteria })),
                quote!(let _ = assert_noul::<#ty>;),
                MockShape::Noul,
            )
        }
        Kind::Choice => {
            let (criteria, mock) = match args.take("labels") {
                Some(expr) => (labels_expr(krate, &expr)?, MockShape::ChoiceLabels),
                None => (quote!(<#ty as #krate::typed::ChoiceCriteria>::criteria()), MockShape::ChoiceTyped),
            };
            args.finish(&["instructions", "name", "labels"])?;
            (
                quote!(#krate::Question::Choice(#krate::Choice { instructions: #instructions, criteria: #criteria })),
                quote!(let _ = assert_choice::<#ty>;),
                mock,
            )
        }
        Kind::Score => {
            let (criteria, mock) = match args.take("levels") {
                Some(expr) => {
                    let (tokens, len) = levels_expr(krate, &expr)?;
                    (tokens, MockShape::ScoreLevels(len))
                }
                None => (quote!(<#ty as #krate::typed::ScoreCriteria>::criteria()), MockShape::ScoreTyped),
            };
            args.finish(&["instructions", "name", "levels"])?;
            (
                quote!(#krate::Question::Score(#krate::Score { instructions: #instructions, criteria: #criteria })),
                quote!(let _ = assert_score::<#ty>;),
                mock,
            )
        }
    };
    Ok(QuestionField { ident, ty, name, question, assert, mock })
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

/// `levels = ["low", "high"]` -> a criteria vector expression and the level count.
fn levels_expr(krate: &TokenStream2, expr: &Expr) -> syn::Result<(TokenStream2, usize)> {
    let Expr::Array(array) = expr else {
        return Err(syn::Error::new_spanned(expr, "`levels` must be an array like [\"low\", \"high\"]"));
    };
    let entries = array.elems.iter().map(|elem| quote!(#krate::Value::from(#elem)));
    Ok((quote!(::std::vec![#(#entries),*]), array.elems.len()))
}

fn kind_asserts(krate: &TokenStream2, asserts: &[TokenStream2]) -> TokenStream2 {
    // Kind checks: referencing the generic fn with an explicit type argument fails to compile
    // when the field type does not implement the marker trait for its question kind.
    quote! {
        const _: () = {
            #[allow(dead_code)]
            fn assert_noul<T: #krate::typed::NoulTarget>() {}
            #[allow(dead_code)]
            fn assert_choice<T: #krate::typed::ChoiceTarget>() {}
            #[allow(dead_code)]
            fn assert_score<T: #krate::typed::ScoreTarget>() {}
            #(#asserts)*
        };
    }
}

/// Setter methods on a generated mock builder for one field, named `<prefix><field>`.
fn mock_setters(krate: &TokenStream2, field: &QuestionField, method_prefix: &str) -> TokenStream2 {
    let name = &field.name;
    let ty = &field.ty;
    let setter = format_ident!("{}{}", method_prefix, field.ident);
    let setter_with = format_ident!("{}{}_with", method_prefix, field.ident);
    let doc = format!("Answer the `{name}` question.");
    let doc_with = format!("Answer the `{name}` question with a full distribution.");
    match field.mock {
        MockShape::Noul => quote! {
            #[doc = #doc]
            pub fn #setter(mut self, probability: f64) -> Self {
                self.response = self.response.noul(#name, probability);
                self
            }
        },
        MockShape::ChoiceTyped => quote! {
            #[doc = #doc]
            pub fn #setter(mut self, label: <#ty as #krate::typed::ChoiceOf>::Labels) -> Self {
                self.response = self.response.choice_typed(#name, label);
                self
            }
            #[doc = #doc_with]
            pub fn #setter_with(
                mut self,
                probabilities: impl ::core::iter::IntoIterator<Item = (<#ty as #krate::typed::ChoiceOf>::Labels, f64)>,
            ) -> Self {
                self.response = self.response.choice_typed_with(#name, probabilities);
                self
            }
        },
        MockShape::ChoiceLabels => quote! {
            #[doc = #doc]
            pub fn #setter(mut self, label: impl ::core::convert::Into<::std::string::String>) -> Self {
                self.response = self.response.choice_label(#name, label);
                self
            }
            #[doc = #doc_with]
            pub fn #setter_with(
                mut self,
                probabilities: impl ::core::iter::IntoIterator<Item = (impl ::core::convert::Into<::std::string::String>, f64)>,
            ) -> Self {
                self.response = self.response.choice(#name, probabilities);
                self
            }
        },
        MockShape::ScoreTyped => quote! {
            #[doc = #doc]
            pub fn #setter(mut self, level: <#ty as #krate::typed::ScoreOf>::Levels) -> Self {
                self.response = self.response.score_typed(#name, level);
                self
            }
            #[doc = #doc_with]
            pub fn #setter_with(
                mut self,
                probabilities: impl ::core::iter::IntoIterator<Item = (<#ty as #krate::typed::ScoreOf>::Levels, f64)>,
            ) -> Self {
                self.response = self.response.score_typed_with(#name, probabilities);
                self
            }
        },
        MockShape::ScoreLevels(len) => {
            let len = len as u32;
            quote! {
                #[doc = #doc]
                pub fn #setter(mut self, level: u32) -> Self {
                    self.response = self.response.score_level(#name, level, #len);
                    self
                }
                #[doc = #doc_with]
                pub fn #setter_with(mut self, probabilities: impl ::core::iter::IntoIterator<Item = (u32, f64)>) -> Self {
                    self.response = self.response.score(#name, probabilities, ::core::iter::empty::<(u32, #krate::Value)>());
                    self
                }
            }
        }
    }
}

/// The shared shell of a generated mock builder.
fn mock_builder(krate: &TokenStream2, owner: &Ident, setters: TokenStream2) -> TokenStream2 {
    let builder = format_ident!("{owner}Mock");
    let doc = format!(
        "A scripted [`MockResponse`]({krate}::testing::MockResponse) answering every question of `{owner}`; built with `{owner}::mock()`."
    );
    quote! {
        #[doc = #doc]
        #[derive(Clone, Debug)]
        pub struct #builder {
            response: #krate::testing::MockResponse,
        }

        impl #owner {
            /// Start a scripted response for this question set.
            pub fn mock() -> #builder {
                #builder { response: #krate::testing::MockResponse::answers() }
            }
        }

        impl #builder {
            #setters

            /// Adjust the underlying response (model, usage, request ID, extra fields).
            pub fn with(
                mut self,
                f: impl ::core::ops::FnOnce(#krate::testing::MockResponse) -> #krate::testing::MockResponse,
            ) -> Self {
                self.response = f(self.response);
                self
            }

            /// The scripted response.
            pub fn build(self) -> #krate::testing::MockResponse {
                self.response
            }
        }

        impl ::core::convert::From<#builder> for #krate::testing::MockResponse {
            fn from(builder: #builder) -> Self {
                builder.response
            }
        }
    }
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
///
/// Struct attributes: `#[questions(crate = "path")]`, and `#[questions(mock)]` to also generate a
/// `<Name>Mock` fixture builder (requires the main crate's `test-util` feature).
#[proc_macro_derive(Questions, attributes(questions, noul, choice, score))]
pub fn derive_questions(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_questions(&input).unwrap_or_else(|error| error.to_compile_error()).into()
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
    let mock = container.as_mut().is_some_and(|args| args.take_flag("mock"));
    if let Some(args) = container {
        args.finish(&["crate", "mock"])?;
    }

    let struct_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mut names = std::collections::HashSet::new();
    let parsed: Vec<QuestionField> =
        fields.named.iter().map(|field| question_field(field, &krate, "", &mut names)).collect::<syn::Result<_>>()?;
    let inserts: Vec<_> = parsed.iter().map(|f| f.insert(&krate)).collect();
    let builds: Vec<_> = parsed.iter().map(|f| f.build(&krate)).collect();
    let asserts: Vec<_> = parsed.iter().map(|f| f.assert.clone()).collect();
    let checks = kind_asserts(&krate, &asserts);

    let mock_impl = if mock {
        let setters: TokenStream2 = parsed.iter().map(|f| mock_setters(&krate, f, "")).collect();
        mock_builder(&krate, struct_name, setters)
    } else {
        quote!()
    };

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

        #checks
        #mock_impl
    })
}

// --- Route --------------------------------------------------------------------------------------

/// Derive `Route` (and `Questions`) for an enum: one choice question selects the variant and
/// each variant's fields are its own questions, all in one request.
///
/// Enum attributes: `#[route("instructions", name = "route", rename_all = "snake_case",
/// crate = "path", mock)]`. Variant attributes: `#[route(label = "...", describe = <expr>)]`.
/// Variants are unit or have named fields; fields take the same `#[noul]`/`#[choice]`/`#[score]`
/// attributes as `Questions`, and are named `<label>.<field>` on the wire unless renamed.
#[proc_macro_derive(Route, attributes(route, noul, choice, score))]
pub fn derive_route(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_route(&input).unwrap_or_else(|error| error.to_compile_error()).into()
}

fn expand_route(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "#[derive(Route)] only supports enums"));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(&input.ident, "#[derive(Route)] needs at least one variant"));
    }
    let mut container = Args::parse_attrs(&input.attrs, "route")?;
    let krate = crate_path(&mut container)?;
    let mut route_name = "route".to_string();
    let mut rule = "snake_case".to_string();
    let mut instructions = quote!(::core::option::Option::None);
    let mut mock = false;
    if let Some(args) = container.as_mut() {
        mock = args.take_flag("mock");
        if let Some(name) = args.take_str("name")? {
            route_name = name;
        }
        if let Some(value) = args.take("rename_all") {
            rule = string_literal(&value)?;
            rename("Probe", &rule, value.span())?;
        }
        let expr = match args.take("instructions") {
            Some(expr) => Some(expr),
            None => args.positional("instructions")?,
        };
        if let Some(expr) = expr {
            instructions = quote!(::core::option::Option::Some(#krate::Value::from(#expr)));
        }
    }
    if let Some(args) = container {
        args.finish(&["instructions", "name", "rename_all", "crate", "mock"])?;
    }

    let enum_name = &input.ident;
    let mut names = std::collections::HashSet::from([route_name.clone()]);
    let mut labels = Vec::new();
    let mut describes = Vec::new();
    let mut inserts = Vec::new();
    let mut asserts = Vec::new();
    let mut arms = Vec::new();
    let mut label_arms = Vec::new();
    let mut mock_setters_all = Vec::new();
    let mut seen_labels = std::collections::HashSet::new();

    for variant in &data.variants {
        let variant_ident = &variant.ident;
        let mut args = Args::parse_attrs(&variant.attrs, "route")?;
        let mut label = rename(&variant_ident.to_string(), &rule, variant_ident.span())?;
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
        if !seen_labels.insert(label.clone()) {
            return Err(syn::Error::new_spanned(variant_ident, format!("duplicate route label {label:?}")));
        }

        let fields: Vec<QuestionField> = match &variant.fields {
            Fields::Unit => Vec::new(),
            Fields::Named(named) => {
                let prefix = format!("{label}.");
                named
                    .named
                    .iter()
                    .map(|f| question_field(f, &krate, &prefix, &mut names))
                    .collect::<syn::Result<_>>()?
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "#[derive(Route)] variants must be unit variants or have named fields",
                ));
            }
        };
        for field in &fields {
            inserts.push(field.insert(&krate));
            asserts.push(field.assert.clone());
        }
        let builds: Vec<_> = fields.iter().map(|f| f.build(&krate)).collect();
        arms.push(match &variant.fields {
            Fields::Unit => quote!(#label => ::core::result::Result::Ok(#enum_name::#variant_ident),),
            _ => quote!(#label => ::core::result::Result::Ok(#enum_name::#variant_ident { #(#builds),* }),),
        });
        label_arms.push(match &variant.fields {
            Fields::Unit => quote!(#enum_name::#variant_ident => #label,),
            _ => quote!(#enum_name::#variant_ident { .. } => #label,),
        });

        if mock {
            let variant_snake = variant_ident.to_string().to_snake_case();
            let selector = format_ident!("{variant_snake}");
            let doc = format!("Select the `{label}` route with probability 1.");
            let route_name_lit = route_name.clone();
            mock_setters_all.push(quote! {
                #[doc = #doc]
                pub fn #selector(mut self) -> Self {
                    self.response = self.response.choice_label(#route_name_lit, #label);
                    self
                }
            });
            let prefix = format!("{variant_snake}_");
            for field in &fields {
                mock_setters_all.push(mock_setters(&krate, field, &prefix));
            }
        }
        labels.push(label);
        describes.push(describe);
    }

    let checks = kind_asserts(&krate, &asserts);
    let mock_impl = if mock {
        let route_name_lit = route_name.clone();
        let label_pairs = labels.iter().map(|l| quote!((#l, f64)));
        let _ = label_pairs;
        let mut setters: TokenStream2 = mock_setters_all.into_iter().collect();
        setters.extend(quote! {
            /// Set the routing choice from a full distribution over labels.
            pub fn route_with(
                mut self,
                probabilities: impl ::core::iter::IntoIterator<Item = (impl ::core::convert::Into<::std::string::String>, f64)>,
            ) -> Self {
                self.response = self.response.choice(#route_name_lit, probabilities);
                self
            }
        });
        mock_builder(&krate, enum_name, setters)
    } else {
        quote!()
    };

    Ok(quote! {
        impl #krate::typed::Questions for #enum_name {
            fn questions() -> ::std::collections::BTreeMap<::std::string::String, #krate::Question> {
                let mut map = ::std::collections::BTreeMap::new();
                map.insert(
                    ::std::string::String::from(#route_name),
                    #krate::Question::Choice(#krate::Choice {
                        instructions: #instructions,
                        criteria: ::std::collections::BTreeMap::from([
                            #((::std::string::String::from(#labels), #describes)),*
                        ]),
                    }),
                );
                #(#inserts)*
                map
            }

            fn from_response(
                response: &#krate::SystemOneResponse,
            ) -> ::core::result::Result<Self, #krate::AnswerError> {
                let choice = <#krate::ChoiceAnswer as #krate::typed::FromAnswer>::from_answer(
                    #route_name,
                    response.answer(#route_name),
                )?;
                match choice.choice.as_str() {
                    #(#arms)*
                    other => ::core::result::Result::Err(#krate::AnswerError::UnknownLabel {
                        name: ::std::string::String::from(#route_name),
                        label: ::std::string::String::from(other),
                    }),
                }
            }
        }

        impl #krate::typed::Route for #enum_name {
            const ROUTE_NAME: &'static str = #route_name;
            const LABELS: &'static [&'static str] = &[#(#labels),*];

            fn label(&self) -> &'static str {
                match self { #(#label_arms)* }
            }
        }

        #checks
        #mock_impl
    })
}

// --- Composite ----------------------------------------------------------------------------------

/// Derive `Composite` for a struct: fields marked `#[weight(w)]` are combined into one score.
///
/// Field attributes: `#[weight(0.4)]`, `#[weight(0.2, label = <expr>)]` to read the probability
/// of one label of a choice field, `#[weight(0.3, invert)]` to use `1 - signal`, and
/// `name = "..."` to rename the signal (the field name by default). Struct attribute:
/// `#[composite(crate = "path")]`.
#[proc_macro_derive(Composite, attributes(composite, weight))]
pub fn derive_composite(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand_composite(&input).unwrap_or_else(|error| error.to_compile_error()).into()
}

fn expand_composite(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "#[derive(Composite)] only supports structs"));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Composite)] only supports structs with named fields",
        ));
    };
    let mut container = Args::parse_attrs(&input.attrs, "composite")?;
    let krate = crate_path(&mut container)?;
    if let Some(args) = container {
        args.finish(&["crate"])?;
    }

    let struct_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mut weights = Vec::new();
    let mut signals = Vec::new();
    let mut names = std::collections::HashSet::new();

    for field in &fields.named {
        let Some(mut args) = Args::parse_attrs(&field.attrs, "weight")? else { continue };
        let ident = field.ident.as_ref().expect("named field");
        let invert = args.take_flag("invert");
        let weight = match args.take("weight") {
            Some(expr) => expr,
            None => args
                .positional("weight")?
                .ok_or_else(|| syn::Error::new_spanned(ident, "#[weight(...)] needs a weight, e.g. #[weight(0.4)]"))?,
        };
        let name = args.take_str("name")?.unwrap_or_else(|| ident.to_string());
        if !names.insert(name.clone()) {
            return Err(syn::Error::new_spanned(ident, format!("duplicate signal name {name:?}")));
        }
        let label = args.take("label");
        args.finish(&["weight", "name", "label", "invert"])?;

        let raw = match label {
            Some(label) => quote!(#krate::composite::LabelSignal::label_signal(&self.#ident, #label)),
            None => quote!(#krate::composite::Signal::signal(&self.#ident)),
        };
        let value = if invert { quote!((#raw).map(|v| 1.0 - v)) } else { raw };
        weights.push(quote!(.set(#name, (#weight) as f64)));
        signals.push(quote!((#name, #value)));
    }
    if weights.is_empty() {
        return Err(syn::Error::new_spanned(
            struct_name,
            "#[derive(Composite)] needs at least one #[weight(...)] field",
        ));
    }

    Ok(quote! {
        impl #impl_generics #krate::composite::Composite for #struct_name #ty_generics #where_clause {
            fn default_weights() -> #krate::composite::Weights {
                #krate::composite::Weights::new() #(#weights)*
            }

            fn signals(&self) -> ::std::vec::Vec<(&'static str, ::core::option::Option<f64>)> {
                ::std::vec![#(#signals),*]
            }
        }
    })
}
