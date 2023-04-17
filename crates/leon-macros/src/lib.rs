use leon::{Item, Template};
use quote::quote;
use syn::{parse_macro_input, LitStr};

#[proc_macro]
pub fn template(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as LitStr).value();

    #[allow(clippy::unnecessary_to_owned)]
    let items = Template::parse(&input)
        .unwrap()
        .items
        .into_owned()
        .into_iter()
        .map(|item| match item {
            Item::Text(text) => quote! {
                ::leon::Item::Text(#text)
            },
            Item::Key(key) => quote! {
                ::leon::Item::Key(#key)
            },
        });

    quote! {
        ::leon::Template::new(
            {
                const ITEMS: &'static [::leon::Item<'static>] = &[
                    #(#items),*
                ];
                ITEMS
            },
            ::core::option::Option::None,
        )
    }
    .into()
}
