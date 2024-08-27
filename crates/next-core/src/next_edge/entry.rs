use anyhow::Result;
use indexmap::indexmap;
use indoc::formatdoc;
use turbo_tasks::{RcStr, Value, Vc};
use turbo_tasks_fs::{File, FileSystemPath};
use turbopack_core::{
    asset::AssetContent, context::AssetContext, module::Module, reference_type::ReferenceType,
    virtual_source::VirtualSource,
};
use turbopack_ecmascript::utils::StringifyJs;

#[turbo_tasks::function]
pub async fn wrap_edge_entry(
    asset_context: Vc<Box<dyn AssetContext>>,
    project_root: Vc<FileSystemPath>,
    entry: Vc<Box<dyn Module>>,
    pathname: RcStr,
) -> Result<Vc<Box<dyn Module>>> {
    // The wrapped module could be an async module, we handle that with the proxy
    // here. The comma expression makes sure we don't call the function with the
    // module as the "this" arg.
    let source = formatdoc!(
        r#"
            self._ENTRIES ||= {{}}
            self._ENTRIES[{}] = import('MODULE')
        "#,
        StringifyJs(&format_args!("middleware_{}", pathname))
    );
    let file = File::from(source);

    // TODO(alexkirsz) Figure out how to name this virtual asset.
    let virtual_source = VirtualSource::new(
        project_root.join("edge-wrapper.js".into()),
        AssetContent::file(file.into()),
    );

    let inner_assets = indexmap! {
        "MODULE".into() => entry
    };

    let module = asset_context
        .process(
            Vc::upcast(virtual_source),
            Value::new(ReferenceType::Internal(Vc::cell(inner_assets))),
        )
        .module();
    Ok(module)
}