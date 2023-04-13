use crate::{preprocessed::IsSkippedAna, Accumulator, MAX_REFS, PROPAGATE_ERROR_ON_BAD_CST_NODE};

use hyper_ast::{
    hashed::SyntaxNodeHashs,
    store::defaults::{LabelIdentifier, NodeIdentifier},
    tree_gen::SubTreeMetrics,
    types::Type,
};

use hyper_ast_gen_ts_cpp::legion as cpp_tree_gen;

pub(crate) fn handle_cpp_file<'stores, 'cache, 'b: 'stores>(
    tree_gen: &mut cpp_tree_gen::CppTreeGen<'stores, 'cache>,
    name: &[u8],
    text: &'b [u8],
) -> Result<cpp_tree_gen::FNode, ()> {
    let tree = match cpp_tree_gen::CppTreeGen::tree_sitter_parse(text) {
        Ok(tree) => tree,
        Err(tree) => {
            log::warn!("bad CST");
            // println!("{}", name);
            log::debug!("{}", tree.root_node().to_sexp());
            if PROPAGATE_ERROR_ON_BAD_CST_NODE {
                return Err(());
            } else {
                tree
            }
        }
    };
    Ok(tree_gen.generate_file(&name, text, tree.walk()))
}

pub struct CppAcc {
    pub(crate) name: String,
    pub(crate) children: Vec<NodeIdentifier>,
    pub(crate) children_names: Vec<LabelIdentifier>,
    pub(crate) metrics: SubTreeMetrics<SyntaxNodeHashs<u32>>,
}

impl CppAcc {
    pub(crate) fn new(name: String) -> Self {
        Self {
            name,
            children_names: Default::default(),
            children: Default::default(),
            // simple: BasicAccumulator::new(kind),
            metrics: Default::default(),
        }
    }
}

impl From<String> for CppAcc {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

impl CppAcc {
    // pub(crate) fn push_file(
    //     &mut self,
    //     name: LabelIdentifier,
    //     full_node: cpp_tree_gen::FNode,
    // ) {
    //     self.children.push(full_node.local.compressed_node.clone());
    //     self.children_names.push(name);
    //     self.metrics.acc(full_node.local.metrics);
    //     full_node
    //         .local
    //         .ana
    //         .unwrap()
    //         .acc(&Type::Directory, &mut self.ana);
    // }
    // pub(crate) fn push(&mut self, name: LabelIdentifier, full_node: cpp_tree_gen::Local) {
    //     self.children.push(full_node.compressed_node);
    //     self.children_names.push(name);
    //     self.metrics.acc(full_node.metrics);

    //     if let Some(ana) = full_node.ana {
    //         if ana.estimated_refs_count() < MAX_REFS && self.skiped_ana == false {
    //             ana.acc(&Type::Directory, &mut self.ana);
    //         } else {
    //             self.skiped_ana = true;
    //         }
    //     }
    // }
    pub(crate) fn push(
        &mut self,
        name: LabelIdentifier,
        full_node: cpp_tree_gen::Local,
        skiped_ana: bool,
    ) {
        self.children.push(full_node.compressed_node);
        self.children_names.push(name);
        self.metrics.acc(full_node.metrics);
    }
}

impl hyper_ast::tree_gen::Accumulator for CppAcc {
    type Node = (LabelIdentifier, (cpp_tree_gen::Local, IsSkippedAna));
    fn push(&mut self, (name, (full_node, skiped_ana)): Self::Node) {
        self.children.push(full_node.compressed_node);
        self.children_names.push(name);
        self.metrics.acc(full_node.metrics);
    }
}

impl Accumulator for CppAcc {
    type Unlabeled = (cpp_tree_gen::Local, IsSkippedAna);
}
