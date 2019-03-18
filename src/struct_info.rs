use syn;

use proc_macro2::TokenStream;
use syn::parse::Error;
use quote::quote;

use crate::field_info::FieldInfo;
use crate::builder_attr::TypeBuilderAttr;
use crate::util::{make_identifier, empty_type, make_punctuated_single, modify_types_generics_hack};
use crate::util::{path_to_single_string, map_only_one};

#[derive(Debug)]
pub struct StructInfo<'a> {
    pub vis: &'a syn::Visibility,
    pub name: &'a syn::Ident,
    pub generics: &'a syn::Generics,
    pub fields: Vec<FieldInfo<'a>>,

    pub builder_attr: TypeBuilderAttr,
    pub builder_name: syn::Ident,
    pub conversion_helper_trait_name: syn::Ident,
    pub conversion_helper_method_name: syn::Ident,
    pub core: syn::Ident,
}

impl<'a> StructInfo<'a> {
    pub fn included_fields(&self) -> impl Iterator<Item = &FieldInfo<'a>> {
        self.fields.iter().filter(|f| !f.builder_attr.exclude)
    }

    pub fn new(ast: &'a syn::DeriveInput, fields: impl Iterator<Item = &'a syn::Field>) -> Result<StructInfo<'a>, Error> {
        let builder_attr = Self::find_builder_attr(&ast)?;
        let builder_name = syn::Ident::new(&match builder_attr.name {
            Some(ref name) => quote!(#name).to_string(),
            None => format!("{}Builder", ast.ident),
        }, proc_macro2::Span::call_site());
        Ok(StructInfo {
            vis: &ast.vis,
            name: &ast.ident,
            generics: &ast.generics,
            fields: fields.enumerate().map(|(i, f)| FieldInfo::new(i, f)).collect::<Result<_, _>>()?,
            builder_attr: builder_attr,
            builder_name: builder_name,
            conversion_helper_trait_name: make_identifier("conversionHelperTrait", &ast.ident),
            conversion_helper_method_name: make_identifier("conversionHelperMethod", &ast.ident),
            core: make_identifier("core", &ast.ident),
        })
    }

    fn find_builder_attr(ast: &syn::DeriveInput) -> Result<TypeBuilderAttr, Error> {
        Ok(map_only_one(&ast.attrs, |attr| {
            if path_to_single_string(&attr.path).as_ref().map(|s| &**s) == Some("builder") {
                Ok(Some(TypeBuilderAttr::new(&attr.tts)?))
            } else {
                Ok(None)
            }
        })?.unwrap_or_else(|| Default::default()))
    }

    fn modify_generics<F: FnMut(&mut syn::Generics)>(&self, mut mutator: F) -> syn::Generics {
        let mut generics = self.generics.clone();
        mutator(&mut generics);
        generics
    }

    pub fn builder_creation_impl(&self) -> Result<TokenStream, Error> {
        let init_empties = {
            let names = self.included_fields().map(|f| f.name);
            quote!(#( #names: () ),*)
        };
        let builder_generics = {
            let names = self.included_fields().map(|f| f.name);
            let generic_idents = self.included_fields().map(|f| &f.generic_ident);
            quote!(#( #names: #generic_idents ),*)
        };
        let StructInfo { ref vis, ref name, ref builder_name, ref core, .. } = *self;
        let (impl_generics, ty_generics, where_clause) = self.generics.split_for_impl();
        let b_generics = self.modify_generics(|g| {
            for field in self.included_fields() {
                g.params.push(field.generic_ty_param());
            }
        });
        let generics_with_empty = modify_types_generics_hack(&ty_generics, |args| {
            for _ in self.included_fields() {
                args.push(syn::GenericArgument::Type(empty_type()));
            }
        });
        let phantom_generics = self.generics.params.iter().map(|param| {
            let t = match param {
                syn::GenericParam::Lifetime(lifetime) => quote!(&#lifetime ()),
                syn::GenericParam::Type(ty) => {
                    let ty = &ty.ident;
                    quote!(#ty)
                },
                syn::GenericParam::Const(cnst) => {
                    let cnst = &cnst.ident;
                    quote!(#cnst)
                },
            };
            quote!(#core::marker::PhantomData<#t>)
        });
        let builder_method_doc = match self.builder_attr.builder_method_doc {
            Some(ref doc) => quote!(#doc),
            None => {
                let doc = format!("
                    Create a builder for building `{name}`.
                    On the builder, call {setters} to set the values of the fields (they accept `Into` values).
                    Finally, call `.build()` to create the instance of `{name}`.
                    ",
                    name=self.name,
                    setters={
                        let mut result = String::new();
                        let mut is_first = true;
                        for field in self.included_fields() {
                            use std::fmt::Write;
                            if is_first {
                                is_first = false;
                            } else {
                                write!(&mut result, ", ").unwrap();
                            }
                            write!(&mut result, "`.{}(...)`", field.name).unwrap();
                            if field.builder_attr.default.is_some() {
                                write!(&mut result, "(optional)").unwrap();
                            }
                        }
                        result
                    });
                quote!(#doc)
            }
        };
        let builder_type_doc = if self.builder_attr.doc {
            match self.builder_attr.builder_type_doc {
                Some(ref doc) => quote!(#[doc = #doc]),
                None => {
                    let doc = format!("Builder for [`{name}`] instances.\n\nSee [`{name}::builder()`] for more info.", name = name);
                    quote!(#[doc = #doc])
                }
            }
        } else {
            quote!(#[doc(hidden)])
        };
        Ok(quote! {
            extern crate core as #core;
            impl #impl_generics #name #ty_generics #where_clause {
                #[doc = #builder_method_doc]
                #[allow(dead_code)]
                #vis fn builder() -> #builder_name #generics_with_empty {
                    #builder_name {
                        _TypedBuilder__phantomGenerics_: #core::default::Default::default(),
                        #init_empties
                    }
                }
            }

            #[must_use]
            #builder_type_doc
            #[allow(dead_code, non_camel_case_types, non_snake_case)]
            #vis struct #builder_name #b_generics {
                _TypedBuilder__phantomGenerics_: (#( #phantom_generics ),*),
                #builder_generics
            }
        })
    }

    // TODO: once the proc-macro crate limitation is lifted, make this an util trait of this
    // crate.
    pub fn conversion_helper_impl(&self) -> Result<TokenStream, Error> {
        let &StructInfo { conversion_helper_trait_name: ref trait_name,
                          conversion_helper_method_name: ref method_name,
                          .. } = self;
        Ok(quote! {
            #[doc(hidden)]
            #[allow(dead_code, non_camel_case_types, non_snake_case)]
            pub trait #trait_name<T> {
                fn #method_name(self, default: T) -> T;
            }

            impl<T> #trait_name<T> for () {
                fn #method_name(self, default: T) -> T {
                    default
                }
            }

            impl<T> #trait_name<T> for (T,) {
                fn #method_name(self, _: T) -> T {
                    self.0
                }
            }
        })
    }

    pub fn field_impl(&self, field: &FieldInfo) -> Result<TokenStream, Error> {
        let StructInfo { ref builder_name, ref core, .. } = *self;
        let other_fields_name =
            self.included_fields().filter(|f| f.ordinal != field.ordinal).map(|f| f.name);
        // not really "value", since we just use to self.name - but close enough.
        let other_fields_value =
            self.included_fields().filter(|f| f.ordinal != field.ordinal).map(|f| f.name);
        let &FieldInfo { name: ref field_name, ty: ref field_type, ref generic_ident, .. } = field;
        let mut ty_generics: Vec<syn::GenericArgument> = self.generics.params.iter().map(|generic_param| {
            match generic_param {
                syn::GenericParam::Type(type_param) => {
                    let ident = type_param.ident.clone();
                    syn::parse(quote!(#ident).into()).unwrap()
                }
                syn::GenericParam::Lifetime(lifetime_def) => {
                    syn::GenericArgument::Lifetime(lifetime_def.lifetime.clone())
                }
                syn::GenericParam::Const(const_param) => {
                    let ident = const_param.ident.clone();
                    syn::parse(quote!(#ident).into()).unwrap()
                }
            }
        }).collect();
        let mut target_generics = ty_generics.clone();
        let generics = self.modify_generics(|g| {
            for f in self.included_fields() {
                if f.ordinal == field.ordinal {
                    ty_generics.push(syn::GenericArgument::Type(empty_type()));
                    target_generics.push(syn::GenericArgument::Type(f.tuplized_type_ty_param()));
                } else {
                    g.params.push(f.generic_ty_param());
                    let generic_argument = syn::GenericArgument::Type(f.type_ident());
                    ty_generics.push(generic_argument.clone());
                    target_generics.push(generic_argument);
                }
            }
        });
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let doc = match field.builder_attr.doc {
            Some(ref doc) => quote!(#[doc = #doc]),
            None => quote!(),
        };
        Ok(quote!{
            #[allow(dead_code, non_camel_case_types, missing_docs)]
            impl #impl_generics #builder_name < #( #ty_generics ),* > #where_clause {
                #doc
                pub fn #field_name<#generic_ident: #core::convert::Into<#field_type>>(self, value: #generic_ident) -> #builder_name < #( #target_generics ),* > {
                    #builder_name {
                        _TypedBuilder__phantomGenerics_: self._TypedBuilder__phantomGenerics_,
                        #field_name: (value.into(),),
                        #( #other_fields_name: self.#other_fields_value ),*
                    }
                }
            }
        })
    }

    pub fn build_method_impl(&self) -> TokenStream {
        let StructInfo { ref name, ref builder_name, .. } = *self;

        let generics = self.modify_generics(|g| {
            for field in self.included_fields() {
                if field.builder_attr.default.is_some() {
                    let trait_ref = syn::TraitBound {
                        paren_token: None,
                        lifetimes: None,
                        modifier: syn::TraitBoundModifier::None,
                        path: syn::PathSegment {
                            ident: self.conversion_helper_trait_name.clone(),
                            arguments: syn::PathArguments::AngleBracketed(
                                syn::AngleBracketedGenericArguments{
                                    colon2_token: None,
                                    lt_token: Default::default(),
                                    args: make_punctuated_single(syn::GenericArgument::Type(field.ty.clone())),
                                    gt_token: Default::default(),
                                })
                        }.into(),
                    };
                    let mut generic_param: syn::TypeParam = field.generic_ident.clone().into();
                    generic_param.bounds.push(trait_ref.into());
                    g.params.push(generic_param.into());
                }
            }
        });
        let (impl_generics, _, _) = generics.split_for_impl();

        let (_, ty_generics, where_clause) = self.generics.split_for_impl();

        let modified_ty_generics = modify_types_generics_hack(&ty_generics, |args| {
            for field in self.included_fields() {
                let required_type = if field.builder_attr.default.is_some() {
                    field.type_ident()
                } else {
                    field.tuplized_type_ty_param()
                };
                args.push(syn::GenericArgument::Type(required_type));
            }
        });

        let ref helper_trait_method_name = self.conversion_helper_method_name;
        // The default_code of a field can refer to earlier-defined fields, which we handle by
        // writing out a bunch of `let` statements first, which can each refer to earlier ones.
        // This means that field ordering may actually be significant, which isn’t ideal. We could
        // relax that restriction by calculating a DAG of field default_code dependencies and
        // reordering based on that, but for now this much simpler thing is a reasonable approach.
        let assignments = self.fields.iter().map(|field| {
            let ref name = field.name;
            if let Some(ref default) = field.builder_attr.default {
                if field.builder_attr.exclude {
                    quote!(let #name = #default;)
                } else {
                    quote!(let #name = self.#name.#helper_trait_method_name(#default);)
                }
            } else {
                quote!(let #name = self.#name.0;)
            }
        });
        let field_names = self.fields.iter().map(|field| field.name);
        let doc = if self.builder_attr.doc {
            match self.builder_attr.build_method_doc {
                Some(ref doc) => quote!(#[doc = #doc]),
                None => {
                    // I’d prefer “a” or “an” to “its”, but determining which is grammatically
                    // correct is roughly impossible.
                    let doc = format!("Finalise the builder and create its [`{}`] instance", name);
                    quote!(#[doc = #doc])
                },
            }
        } else {
            quote!()
        };
        quote!(
            #[allow(dead_code, non_camel_case_types, missing_docs)]
            impl #impl_generics #builder_name #modified_ty_generics #where_clause {
                #doc
                pub fn build(self) -> #name #ty_generics {
                    #( #assignments )*
                    #name {
                        #( #field_names ),*
                    }
                }
            }
        ).into()
    }
}
