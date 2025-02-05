use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cairo_lang_defs::plugin::{GeneratedFileAuxData, MacroPlugin, PluginResult};
use cairo_lang_diagnostics::DiagnosticEntry;
use cairo_lang_semantic::db::SemanticGroup;
use cairo_lang_semantic::patcher::{Patches, RewriteNode};
use cairo_lang_semantic::plugin::{
    AsDynGeneratedFileAuxData, AsDynMacroPlugin, PluginAuxData, PluginMappedDiagnostic,
    SemanticPlugin,
};
use cairo_lang_semantic::SemanticDiagnostic;
use cairo_lang_syntax::node::db::SyntaxGroup;
use cairo_lang_syntax::node::helpers::QueryAttrs;
use cairo_lang_syntax::node::{ast, Terminal};
use dojo_project::WorldConfig;
use smol_str::SmolStr;
use starknet::core::crypto::pedersen_hash;
use starknet::core::types::FieldElement;

use crate::component::{
    handle_component_impl, handle_component_struct, handle_generated_component,
};
use crate::system::handle_system;

const COMPONENT_ATTR: &str = "generated_component";
const SYSTEM_ATTR: &str = "system";

/// Dojo related auxiliary data of the Dojo plugin.
#[derive(Debug, PartialEq, Eq)]
pub struct DojoAuxData {
    /// Patches of code that need translation in case they have diagnostics.
    pub patches: Patches,

    /// A list of components that were processed by the plugin.
    pub components: Vec<smol_str::SmolStr>,
    /// A list of systems that were processed by the plugin.
    pub systems: Vec<smol_str::SmolStr>,
}
impl GeneratedFileAuxData for DojoAuxData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn eq(&self, other: &dyn GeneratedFileAuxData) -> bool {
        if let Some(other) = other.as_any().downcast_ref::<Self>() { self == other } else { false }
    }
}
impl AsDynGeneratedFileAuxData for DojoAuxData {
    fn as_dyn_macro_token(&self) -> &(dyn GeneratedFileAuxData + 'static) {
        self
    }
}
impl PluginAuxData for DojoAuxData {
    fn map_diag(
        &self,
        db: &(dyn SemanticGroup + 'static),
        diag: &dyn std::any::Any,
    ) -> Option<PluginMappedDiagnostic> {
        let Some(diag) = diag.downcast_ref::<SemanticDiagnostic>() else {return None;};
        let span = self
            .patches
            .translate(db.upcast(), diag.stable_location.diagnostic_location(db.upcast()).span)?;
        Some(PluginMappedDiagnostic { span, message: diag.format(db) })
    }
}

#[cfg(test)]
#[path = "plugin_test.rs"]
mod test;

#[derive(Debug, Default)]
pub struct DojoPlugin {
    pub world_config: WorldConfig,
    pub impls: Arc<Mutex<HashMap<SmolStr, Vec<RewriteNode>>>>,
}

impl DojoPlugin {
    pub fn new(world_config: WorldConfig) -> Self {
        Self { world_config, impls: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn handle_mod(&self, db: &dyn SyntaxGroup, module_ast: ast::ItemModule) -> PluginResult {
        if module_ast.has_attr(db, COMPONENT_ATTR) {
            let name = module_ast.name(db).text(db);
            let guard = self.impls.lock().unwrap();
            let impls = guard.get(&name).map_or_else(Vec::new, |vec| vec.clone());
            return handle_generated_component(db, module_ast, impls);
        }

        PluginResult::default()
    }
}

impl MacroPlugin for DojoPlugin {
    fn generate_code(&self, db: &dyn SyntaxGroup, item_ast: ast::Item) -> PluginResult {
        match item_ast {
            ast::Item::Module(module_ast) => self.handle_mod(db, module_ast),
            ast::Item::Struct(struct_ast) => {
                for attr in struct_ast.attributes(db).elements(db) {
                    if attr.attr(db).text(db) == "derive" {
                        if let ast::OptionAttributeArgs::AttributeArgs(args) = attr.args(db) {
                            for arg in args.arg_list(db).elements(db) {
                                if let ast::Expr::Path(expr) = arg {
                                    if let [ast::PathSegment::Simple(segment)] =
                                        &expr.elements(db)[..]
                                    {
                                        let derived = segment.ident(db).text(db);
                                        if matches!(derived.as_str(), "Component") {
                                            let mut guard = self.impls.lock().unwrap();
                                            guard
                                                .entry(struct_ast.name(db).text(db))
                                                .or_insert(vec![]);
                                            return handle_component_struct(db, struct_ast);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                PluginResult::default()
            }
            ast::Item::Impl(impl_ast) => {
                let name = impl_ast.name(db).text(db);
                let mut guard = self.impls.lock().unwrap();

                if guard.get_mut(&name).is_some() {
                    if let ast::MaybeImplBody::Some(body) = impl_ast.body(db) {
                        let rewrite_nodes = handle_component_impl(db, body);
                        guard
                            .entry(name)
                            .and_modify(|vec| vec.extend(rewrite_nodes.to_vec()))
                            .or_insert(vec![]);
                        return PluginResult {
                            remove_original_item: true,
                            ..PluginResult::default()
                        };
                    }
                }

                PluginResult::default()
            }
            ast::Item::FreeFunction(function_ast) => {
                if function_ast.has_attr(db, SYSTEM_ATTR) {
                    return handle_system(db, self.world_config, function_ast);
                }

                PluginResult::default()
            }
            _ => PluginResult::default(),
        }
    }
}

impl AsDynMacroPlugin for DojoPlugin {
    fn as_dyn_macro_plugin<'a>(self: Arc<Self>) -> Arc<dyn MacroPlugin + 'a>
    where
        Self: 'a,
    {
        self
    }
}
impl SemanticPlugin for DojoPlugin {}

pub fn get_contract_address(
    module_name: &str,
    class_hash: FieldElement,
    world_address: FieldElement,
) -> FieldElement {
    let mut module_name_32_u8: [u8; 32] = [0; 32];
    module_name_32_u8[32 - module_name.len()..].copy_from_slice(module_name.as_bytes());

    let salt = pedersen_hash(
        &FieldElement::ZERO,
        &FieldElement::from_bytes_be(&module_name_32_u8).unwrap(),
    );
    starknet::core::utils::get_contract_address(salt, class_hash, &[], world_address)
}
