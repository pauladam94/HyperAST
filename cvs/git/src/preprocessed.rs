use std::{
    collections::{BTreeMap, HashMap},
    iter::Peekable,
    path::{Components, PathBuf},
    time::Instant,
};

use git2::{Oid, Repository};
use hyper_ast::{
    filter::{Bloom, BF},
    hashed::{self, SyntaxNodeHashs},
    position::{extract_position, Position, StructuralPosition},
    store::{
        labels::DefaultLabelIdentifier,
        nodes::legion::{compo, EntryRef, NodeStore, CS},
        nodes::{legion, DefaultNodeIdentifier as NodeIdentifier},
    },
    tree_gen::SubTreeMetrics,
    types::{LabelStore as _, Labeled, Tree, Type, Typed, WithChildren},
};
use log::info;
use rusted_gumtree_gen_ts_java::{
    filter::BloomSize,
    impact::{
        declaration::ExplorableDecl,
        element::{ExplorableRef, IdentifierFormat, LabelPtr, RefPtr, RefsEnum},
        partial_analysis::PartialAnalysis,
        usage::{self, remake_pkg_ref, IterDeclarations},
    },
    java_tree_gen_full_compress_legion_ref::{self, hash32},
};

use crate::{
    git::{all_commits_between, BasicGitObjects},
    java::{handle_java_file, JavaAcc},
    maven::{handle_pom_file, IterMavenModules, MavenModuleAcc, POM},
    Commit, Diffs, Impacts, SimpleStores, MAX_REFS, MD,
};
use rusted_gumtree_gen_ts_java::java_tree_gen_full_compress_legion_ref as java_tree_gen;
use rusted_gumtree_gen_ts_xml::xml_tree_gen::{self, XmlTreeGen};
use tuples::CombinConcat;

pub struct PreProcessedRepository {
    name: String,
    pub(crate) main_stores: SimpleStores,
    java_md_cache: java_tree_gen::MDCache,
    pub object_map: BTreeMap<git2::Oid, (hyper_ast::store::nodes::DefaultNodeIdentifier, MD)>,
    pub object_map_pom: BTreeMap<git2::Oid, POM>,
    pub object_map_java: BTreeMap<git2::Oid, (java_tree_gen::Local, bool)>,
    pub commits: HashMap<git2::Oid, Commit>,
}

impl PreProcessedRepository {
    pub fn main_stores(&mut self) -> &mut SimpleStores {
        &mut self.main_stores
    }

    fn is_handled(name: &Vec<u8>) -> bool {
        name.ends_with(b".java") || name.ends_with(b".xml")
    }

    pub fn get_or_insert_label(
        &mut self,
        s: &str,
    ) -> hyper_ast::store::labels::DefaultLabelIdentifier {
        use hyper_ast::types::LabelStore;
        self.main_stores.label_store.get_or_insert(s)
    }

    pub fn print_refs(&self, ana: &PartialAnalysis) {
        ana.print_refs(&self.main_stores.label_store);
    }

    fn xml_generator(&mut self) -> XmlTreeGen {
        XmlTreeGen {
            line_break: "\n".as_bytes().to_vec(),
            stores: &mut self.main_stores,
        }
    }

    fn java_generator(&mut self, text: &[u8]) -> java_tree_gen::JavaTreeGen {
        let line_break = if text.contains(&b"\r"[0]) {
            "\r\n".as_bytes().to_vec()
        } else {
            "\n".as_bytes().to_vec()
        };
        java_tree_gen::JavaTreeGen {
            line_break,
            stores: &mut self.main_stores,
            md_cache: &mut self.java_md_cache,
        }
    }

    pub fn purge_caches(&mut self) {
        self.java_md_cache.clear()
    }
}

impl PreProcessedRepository {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn new(name: &str) -> PreProcessedRepository {
        let name = name.to_owned();
        PreProcessedRepository {
            name,
            main_stores: SimpleStores::default(),
            java_md_cache: Default::default(),
            object_map: BTreeMap::default(),
            object_map_pom: BTreeMap::default(),
            object_map_java: BTreeMap::default(),
            commits: Default::default(),
        }
    }

    pub fn pre_process(
        &mut self,
        repository: &mut Repository,
        before: &str,
        after: &str,
        dir_path: &str,
    ) {
        println!(
            "commits to process: {}",
            all_commits_between(&repository, before, after).count()
        );
        let rw = all_commits_between(&repository, before, after);
        rw
            // .skip(1500)release-1.0.0 refs/tags/release-3.3.2-RC4
            .take(2)
            .for_each(|oid| {
                let oid = oid.unwrap();
                let c = self.handle_maven_commit(&repository, dir_path, oid);
                self.commits.insert(oid.clone(), c);
            });
    }

    pub fn pre_process_no_maven(
        &mut self,
        repository: &mut Repository,
        before: &str,
        after: &str,
        dir_path: &str,
    ) {
        println!(
            "commits to process: {}",
            all_commits_between(&repository, before, after).count()
        );
        let rw = all_commits_between(&repository, before, after);
        rw
            // .skip(1500)release-1.0.0 refs/tags/release-3.3.2-RC4
            .take(2)
            .for_each(|oid| {
                let oid = oid.unwrap();
                let c = self.handle_java_commit(&repository, dir_path, oid);
                self.commits.insert(oid.clone(), c);
            });
    }

    // pub fn first(before: &str, after: &str) -> Diffs {
    //     todo!()
    // }

    // pub fn compute_diff(before: &str, after: &str) -> Diffs {
    //     todo!()
    // }

    // pub fn compute_impacts(diff: &Diffs) -> Impacts {
    //     todo!()
    // }

    // pub fn find_declaration(reff: ExplorableRef) {
    //     todo!()
    // }

    // pub fn find_references(decl: ExplorableDecl) {
    //     todo!()
    // }

    /// module_path: path to wanted root module else ""
    fn handle_maven_commit(
        &mut self,
        repository: &Repository,
        module_path: &str,
        commit_oid: git2::Oid,
    ) -> Commit {
        let dir_path = PathBuf::from(module_path);
        let mut dir_path = dir_path.components().peekable();
        let commit = repository.find_commit(commit_oid).unwrap();
        let tree = commit.tree().unwrap();

        info!("handle commit: {}", commit_oid);
        let root_full_node = self.handle_maven_module(repository, &mut dir_path, b"", tree.id());
        // let root_full_node = self.fast_fwd(repository, &mut dir_path, b"", tree.id()); // used to directly access specific java sources
        Commit {
            meta_data: root_full_node.1,
            parents: commit.parents().into_iter().map(|x| x.id()).collect(),
            ast_root: root_full_node.0,
        }
    }

    fn handle_java_commit(
        &mut self,
        repository: &Repository,
        module_path: &str,
        commit_oid: git2::Oid,
    ) -> Commit {
        let dir_path = PathBuf::from(module_path);
        let mut dir_path = dir_path.components().peekable();
        let commit = repository.find_commit(commit_oid).unwrap();
        let tree = commit.tree().unwrap();

        info!("handle commit: {}", commit_oid);
        let root_full_node = self.fast_fwd(repository, &mut dir_path, b"", tree.id()); // used to directly access specific java sources
        Commit {
            meta_data: root_full_node.1,
            parents: commit.parents().into_iter().map(|x| x.id()).collect(),
            ast_root: root_full_node.0,
        }
    }

    fn fast_fwd(
        &mut self,
        repository: &Repository,
        mut dir_path: &mut Peekable<Components>,
        name: &[u8],
        oid: git2::Oid,
    ) -> (NodeIdentifier, MD) {
        let dir_hash = hash32(&Type::MavenDirectory);
        let root_full_node;
        let tree = repository.find_tree(oid).unwrap();

        /// sometimes order of files/dirs can be important, similarly to order of statement
        /// exploration order for example
        fn prepare_dir_exploration(
            tree: git2::Tree,
            dir_path: &mut Peekable<Components>,
        ) -> Vec<BasicGitObjects> {
            let mut children_objects: Vec<BasicGitObjects> = tree.iter().map(Into::into).collect();
            if dir_path.peek().is_none() {
                let p = children_objects.iter().position(|x| match x {
                    BasicGitObjects::Blob(_, n) => n.eq(b"pom.xml"),
                    _ => false,
                });
                if let Some(p) = p {
                    children_objects.swap(0, p); // priority to pom.xml processing
                    children_objects.reverse(); // we use it like a stack
                }
            }
            children_objects
        }
        let prepared = prepare_dir_exploration(tree, &mut dir_path);
        let mut stack: Vec<(Oid, Vec<BasicGitObjects>, MavenModuleAcc)> = vec![(
            oid,
            prepared,
            MavenModuleAcc::new(std::str::from_utf8(&name).unwrap().to_string()),
        )];
        loop {
            if let Some(current_dir) = stack.last_mut().expect("never empty").1.pop() {
                match current_dir {
                    BasicGitObjects::Tree(x, name) => {
                        if let Some(s) = dir_path.peek() {
                            if name.eq(std::os::unix::prelude::OsStrExt::as_bytes(s.as_os_str())) {
                                dir_path.next();
                                stack.last_mut().expect("never empty").1.clear();
                                let tree = repository.find_tree(x).unwrap();
                                let prepared = prepare_dir_exploration(tree, &mut dir_path);
                                stack.push((
                                    x,
                                    prepared,
                                    MavenModuleAcc::new(
                                        std::str::from_utf8(&name).unwrap().to_string(),
                                    ),
                                ));
                                continue;
                            } else {
                                continue;
                            }
                        } else {
                            if let Some(already) = self.object_map.get(&x) {
                                // reinit already computed node for post order
                                let full_node = already.clone();

                                let name = self
                                    .main_stores()
                                    .label_store
                                    .get_or_insert(std::str::from_utf8(&name).unwrap());
                                let n = self.main_stores().node_store.resolve(full_node.0);
                                let already_name = *n.get_label();
                                if name != already_name {
                                    let already_name = self
                                        .main_stores()
                                        .label_store
                                        .resolve(&already_name)
                                        .to_string();
                                    let name = self.main_stores().label_store.resolve(&name);
                                    panic!("{} != {}", name, already_name);
                                } else if stack.is_empty() {
                                    root_full_node = full_node;
                                    break;
                                } else {
                                    let w = &mut stack.last_mut().unwrap().2;
                                    assert!(!w.children_names.contains(&name));
                                    w.push_submodule(name, full_node);
                                    continue;
                                }
                            }
                            let tree = repository.find_tree(x).unwrap();
                            let full_node = self.handle_java_src(repository, &name, tree.id());
                            let paren_acc = &mut stack.last_mut().unwrap().2;
                            let name = self
                                .main_stores()
                                .label_store
                                .get_or_insert(std::str::from_utf8(&name).unwrap());
                            assert!(!paren_acc.children_names.contains(&name));
                            paren_acc.push_source_directory(name, full_node);
                        }
                    }
                    BasicGitObjects::Blob(_, _) => {
                        continue;
                    }
                }
            } else if let Some((id, _, mut acc)) = stack.pop() {
                // commit node
                let hashed_label = hash32(&acc.name);
                let hsyntax = hashed::inner_node_hash(
                    &dir_hash,
                    &0,
                    &acc.metrics.size,
                    &acc.metrics.hashs.syntax,
                );
                let label = self
                    .main_stores()
                    .label_store
                    .get_or_insert(acc.name.clone());

                let eq = |x: EntryRef| {
                    let t = x.get_component::<Type>().ok();
                    if &t != &Some(&Type::MavenDirectory) {
                        return false;
                    }
                    let l = x.get_component::<java_tree_gen::LabelIdentifier>().ok();
                    if l != Some(&label) {
                        return false;
                    } else {
                        let cs = x.get_component::<Vec<NodeIdentifier>>().ok();
                        let r = cs == Some(&acc.children);
                        if !r {
                            return false;
                        }
                    }
                    true
                };
                let ana = {
                    let new_sub_modules = drain_filter_strip(&mut acc.sub_modules, b"..");
                    let new_main_dirs = drain_filter_strip(&mut acc.main_dirs, b"..");
                    let new_test_dirs = drain_filter_strip(&mut acc.test_dirs, b"..");
                    let ana = acc.ana;
                    if !new_sub_modules.is_empty()
                        || !new_main_dirs.is_empty()
                        || !new_test_dirs.is_empty()
                    {
                        println!(
                            "{:?} {:?} {:?}",
                            new_sub_modules, new_main_dirs, new_test_dirs
                        );
                        todo!("also prepare search for modules and sources in parent, should also tell from which module it is required");
                    }
                    // println!("refs in directory");
                    // println!("ref count in dir {}", ana.refs_count());
                    // ana.print_refs(self.main_stores().label_store);
                    // println!("decls in directory");
                    // ana.print_decls(self.main_stores().label_store);
                    ana.resolve()
                };
                // println!("ref count in dir after resolver {}", ana.refs_count());
                // println!("refs in directory after resolve");
                // ana.print_refs(self.main_stores().label_store);
                let insertion = self
                    .main_stores()
                    .node_store
                    .prepare_insertion(&hsyntax, eq);
                let hashs = SyntaxNodeHashs {
                    structt: hashed::inner_node_hash(
                        &dir_hash,
                        &0,
                        &acc.metrics.size,
                        &acc.metrics.hashs.structt,
                    ),
                    label: hashed::inner_node_hash(
                        &dir_hash,
                        &hashed_label,
                        &acc.metrics.size,
                        &acc.metrics.hashs.label,
                    ),
                    syntax: hsyntax,
                };
                let node_id = if let Some(id) = insertion.occupied_id() {
                    id
                } else {
                    println!("make mm {} {}", &acc.name, acc.children.len());
                    let vacant = insertion.vacant();
                    assert_eq!(acc.children_names.len(),acc.children.len());
                    NodeStore::insert_after_prepare(
                        vacant,
                        (
                            Type::MavenDirectory,
                            label,
                            hashs,
                            CS(acc.children_names),
                            CS(acc.children),
                            BloomSize::Much,
                        ),
                    )
                };

                {
                    let n = self.main_stores.node_store.resolve(node_id);
                    if !n.has_children() {
                        println!(
                            "z {} {:?} {:?} {:?} {:?}",
                            n.get_component::<CS<NodeIdentifier>>().is_ok(),
                            n.get_component::<CS<NodeIdentifier>>()
                                .map_or(&CS(vec![]), |x| x),
                            n.get_component::<CS<NodeIdentifier>>().map(|x| x.0.len()),
                            n.has_children(),
                            n.get_component::<CS<NodeIdentifier>>()
                                .map(|x| !x.0.is_empty())
                                .unwrap_or(false)
                        );
                    }
                }

                let metrics = SubTreeMetrics {
                    size: acc.metrics.size + 1,
                    height: acc.metrics.height + 1,
                    hashs,
                };

                let full_node = (
                    node_id.clone(),
                    MD {
                        metrics: metrics,
                        ana,
                    },
                );

                self.object_map.insert(id, full_node.clone());

                if stack.is_empty() {
                    root_full_node = full_node;
                    break;
                } else {
                    println!("dir: {}", &acc.name);
                    let w = &mut stack.last_mut().unwrap().2;
                    let name = self
                        .main_stores()
                        .label_store
                        .get_or_insert(acc.name);
                    assert!(!w.children_names.contains(&name));
                    w.push_submodule(name, full_node);
                }
            } else {
                panic!("never empty")
            }
        }
        root_full_node
    }

    fn handle_maven_module(
        &mut self,
        repository: &Repository,
        mut dir_path: &mut Peekable<Components>,
        name: &[u8],
        oid: git2::Oid,
    ) -> (NodeIdentifier, MD) {
        // use java_tree_gen::{hash32, EntryR, NodeIdentifier, NodeStore,};

        let dir_hash = hash32(&Type::MavenDirectory);
        let root_full_node;
        let tree = repository.find_tree(oid).unwrap();

        /// sometimes order of files/dirs can be important, similarly to order of statement
        /// exploration order for example
        fn prepare_dir_exploration(
            tree: git2::Tree,
            dir_path: &mut Peekable<Components>,
        ) -> Vec<BasicGitObjects> {
            let mut children_objects: Vec<BasicGitObjects> = tree.iter().map(Into::into).collect();
            if dir_path.peek().is_none() {
                let p = children_objects.iter().position(|x| match x {
                    BasicGitObjects::Blob(_, n) => n.eq(b"pom.xml"),
                    _ => false,
                });
                if let Some(p) = p {
                    children_objects.swap(0, p); // priority to pom.xml processing
                    children_objects.reverse(); // we use it like a stack
                }
            }
            children_objects
        }
        let prepared = prepare_dir_exploration(tree, &mut dir_path);
        let mut stack: Vec<(Oid, Vec<BasicGitObjects>, MavenModuleAcc)> = vec![(
            oid,
            prepared,
            MavenModuleAcc::new(std::str::from_utf8(&name).unwrap().to_string()),
        )];
        loop {
            if let Some(current_dir) = stack.last_mut().expect("never empty").1.pop() {
                match current_dir {
                    BasicGitObjects::Tree(x, name) => {
                        if let Some(s) = dir_path.peek() {
                            if name.eq(std::os::unix::prelude::OsStrExt::as_bytes(s.as_os_str())) {
                                dir_path.next();
                                stack.last_mut().expect("never empty").1.clear();
                                let tree = repository.find_tree(x).unwrap();
                                let prepared = prepare_dir_exploration(tree, &mut dir_path);
                                stack.push((
                                    x,
                                    prepared,
                                    MavenModuleAcc::new(
                                        std::str::from_utf8(&name).unwrap().to_string(),
                                    ),
                                ));
                                continue;
                            } else {
                                continue;
                            }
                        }
                        // println!("h tree {:?}", std::str::from_utf8(&name));
                        // check if module or src/main/java or src/test/java
                        if let Some(already) = self.object_map.get(&x) {
                            // reinit already computed node for post order
                            let full_node = already.clone();

                            if stack.is_empty() {
                                root_full_node = full_node;
                                break;
                            } else {
                                let w = &mut stack.last_mut().unwrap().2;
                                let name = self
                                    .main_stores()
                                    .label_store
                                    .get_or_insert(std::str::from_utf8(&name).unwrap());
                                assert!(!w.children_names.contains(&name));
                                w.push_submodule(name, full_node);
                            }
                            continue;
                        }
                        // TODO use maven pom.xml to find source_dir  and tests_dir ie. ignore resources, maybe also tests
                        // TODO maybe at some point try to handle maven modules and source dirs that reference parent directory in their path
                        println!("mm tree {:?}", std::str::from_utf8(&name));
                        let tree = repository.find_tree(x).unwrap();

                        let parent_acc = &mut stack.last_mut().unwrap().2;
                        // println!(
                        //     "{} source_dirs {:?}",
                        //     std::str::from_utf8(&name).unwrap(),
                        //     parent_acc.main_dirs
                        // );
                        let mut new_sub_modules =
                            drain_filter_strip(&mut parent_acc.sub_modules, &name);
                        let mut new_main_dirs =
                            drain_filter_strip(&mut parent_acc.main_dirs, &name);
                        let mut new_test_dirs =
                            drain_filter_strip(&mut parent_acc.test_dirs, &name);

                        // println!("matched source_dirs {:?}", new_main_dirs);

                        let is_source_dir = new_main_dirs
                            .drain_filter(|x| x.components().next().is_none())
                            .count()
                            > 0;
                        let is_test_source_dir = new_test_dirs
                            .drain_filter(|x| x.components().next().is_none())
                            .count()
                            > 0;
                        if is_source_dir || is_test_source_dir {
                            // handle as source dir
                            let full_node = self.handle_java_src(repository, &name, tree.id());
                            let paren_acc = &mut stack.last_mut().unwrap().2;
                            let name = self
                                .main_stores()
                                .label_store
                                .get_or_insert(std::str::from_utf8(&name).unwrap());
                            assert!(!paren_acc.children_names.contains(&name));
                            if is_source_dir {
                                paren_acc.push_source_directory(name, full_node);
                            } else {
                                // is_test_source_dir
                                paren_acc.push_test_source_directory(name, full_node);
                            }
                        }

                        let is_maven_module = new_sub_modules
                            .drain_filter(|x| x.components().next().is_none())
                            .count()
                            > 0;
                        // println!(
                        //     "{} {} {}",
                        //     is_source_dir, is_test_source_dir, is_maven_module
                        // );
                        // TODO check it we can use more info from context and prepare analysis more specifically
                        if is_maven_module
                            || !new_sub_modules.is_empty()
                            || !new_main_dirs.is_empty()
                            || !new_test_dirs.is_empty()
                        {
                            let prepared = prepare_dir_exploration(tree, &mut dir_path);
                            if is_maven_module {
                                // handle as maven module
                                stack.push((
                                    x,
                                    prepared,
                                    MavenModuleAcc::with_content(
                                        std::str::from_utf8(&name).unwrap().to_string(),
                                        new_sub_modules,
                                        new_main_dirs,
                                        new_test_dirs,
                                    ),
                                ));
                            } else {
                                // search further inside
                                stack.push((
                                    x,
                                    prepared,
                                    MavenModuleAcc::with_content(
                                        std::str::from_utf8(&name).unwrap().to_string(),
                                        new_sub_modules,
                                        new_main_dirs,
                                        new_test_dirs,
                                    ),
                                ));
                            };
                        } else if !(is_source_dir || is_test_source_dir) {
                            // anyway try to find maven modules, but maybe can do better
                            let prepared = prepare_dir_exploration(tree, &mut dir_path);
                            stack.push((
                                x,
                                prepared,
                                MavenModuleAcc::with_content(
                                    std::str::from_utf8(&name).unwrap().to_string(),
                                    new_sub_modules,
                                    new_main_dirs,
                                    new_test_dirs,
                                ),
                            ));
                        }
                    }
                    BasicGitObjects::Blob(x, name) => {
                        if dir_path.peek().is_some() {
                            continue;
                        } else if name.eq(b"pom.xml") {
                            if let Some(already) = self.object_map_pom.get(&x) {
                                // TODO reinit already computed node for post order
                                let full_node = already.clone();
                                let w = &mut stack.last_mut().unwrap().2;
                                let name = self
                                    .main_stores()
                                    .label_store
                                    .get_or_insert(std::str::from_utf8(&name).unwrap());
                                assert!(!w.children_names.contains(&name));
                                w.push_pom(name, full_node);
                                continue;
                            }
                            println!("blob {:?}", std::str::from_utf8(&name));
                            let a = repository.find_blob(x).unwrap();
                            if let Ok(z) = std::str::from_utf8(a.content()) {
                                // println!("content: {}", z);
                                let text = a.content();
                                let parent_acc = &mut stack.last_mut().unwrap().2;

                                // let g = XmlTreeGen {
                                //     line_break: "\n".as_bytes().to_vec(),
                                //     stores: self.main_stores,
                                // };
                                // let full_node =
                                //     handle_pom_file(&mut g, &name, text);
                                let full_node =
                                    handle_pom_file(&mut self.xml_generator(), &name, text);
                                let x = full_node.unwrap();
                                self.object_map_pom.insert(a.id(), x.clone());
                                let name = self
                                    .main_stores()
                                    .label_store
                                    .get_or_insert(std::str::from_utf8(&name).unwrap());
                                assert!(!parent_acc.children_names.contains(&name));
                                parent_acc.push_pom(name, x);
                            }
                        }
                    }
                }
            } else if let Some((id, _, mut acc)) = stack.pop() {
                // commit node
                let hashed_label = hash32(&acc.name);
                let hsyntax = hashed::inner_node_hash(
                    &dir_hash,
                    &0,
                    &acc.metrics.size,
                    &acc.metrics.hashs.syntax,
                );
                let label = self
                    .main_stores()
                    .label_store
                    .get_or_insert(acc.name.clone());

                let eq = |x: EntryRef| {
                    let t = x.get_component::<Type>().ok();
                    if &t != &Some(&Type::MavenDirectory) {
                        return false;
                    }
                    let l = x.get_component::<java_tree_gen::LabelIdentifier>().ok();
                    if l != Some(&label) {
                        return false;
                    } else {
                        let cs = x.get_component::<Vec<NodeIdentifier>>().ok();
                        let r = cs == Some(&acc.children);
                        if !r {
                            return false;
                        }
                    }
                    true
                };
                let ana = {
                    let new_sub_modules = drain_filter_strip(&mut acc.sub_modules, b"..");
                    let new_main_dirs = drain_filter_strip(&mut acc.main_dirs, b"..");
                    let new_test_dirs = drain_filter_strip(&mut acc.test_dirs, b"..");
                    let ana = acc.ana;
                    if !new_sub_modules.is_empty()
                        || !new_main_dirs.is_empty()
                        || !new_test_dirs.is_empty()
                    {
                        println!(
                            "{:?} {:?} {:?}",
                            new_sub_modules, new_main_dirs, new_test_dirs
                        );
                        todo!("also prepare search for modules and sources in parent, should also tell from which module it is required");
                    }
                    // println!("refs in directory");
                    // println!("ref count in dir {}", ana.refs_count());
                    // ana.print_refs(self.main_stores().label_store);
                    // println!("decls in directory");
                    // ana.print_decls(self.main_stores().label_store);
                    ana.resolve()
                };
                // println!("ref count in dir after resolver {}", ana.refs_count());
                // println!("refs in directory after resolve");
                // ana.print_refs(self.main_stores().label_store);
                let insertion = self
                    .main_stores()
                    .node_store
                    .prepare_insertion(&hsyntax, eq);
                let hashs = SyntaxNodeHashs {
                    structt: hashed::inner_node_hash(
                        &dir_hash,
                        &0,
                        &acc.metrics.size,
                        &acc.metrics.hashs.structt,
                    ),
                    label: hashed::inner_node_hash(
                        &dir_hash,
                        &hashed_label,
                        &acc.metrics.size,
                        &acc.metrics.hashs.label,
                    ),
                    syntax: hsyntax,
                };
                let node_id = if let Some(id) = insertion.occupied_id() {
                    id
                } else {
                    println!("make mm {} {}", &acc.name, acc.children.len());
                    let vacant = insertion.vacant();
                    assert_eq!(acc.children_names.len(),acc.children.len());
                    NodeStore::insert_after_prepare(
                        vacant,
                        (
                            Type::MavenDirectory,
                            label,
                            hashs,
                            CS(acc.children_names), // TODO extract dir names
                            CS(acc.children),
                            BloomSize::Much,
                        ),
                    )
                };

                {
                    let n = self.main_stores.node_store.resolve(node_id);
                    if !n.has_children() {
                        println!(
                            "z {} {:?} {:?} {:?} {:?}",
                            n.get_component::<CS<NodeIdentifier>>().is_ok(),
                            n.get_component::<CS<NodeIdentifier>>()
                                .map_or(&CS(vec![]), |x| x),
                            n.get_component::<CS<NodeIdentifier>>().map(|x| x.0.len()),
                            n.has_children(),
                            n.get_component::<CS<NodeIdentifier>>()
                                .map(|x| !x.0.is_empty())
                                .unwrap_or(false)
                        );
                    }
                }

                let metrics = SubTreeMetrics {
                    size: acc.metrics.size + 1,
                    height: acc.metrics.height + 1,
                    hashs,
                };

                let full_node = (
                    node_id.clone(),
                    MD {
                        metrics: metrics,
                        ana,
                    },
                );

                self.object_map.insert(id, full_node.clone());

                if stack.is_empty() {
                    root_full_node = full_node;
                    break;
                } else {
                    let w = &mut stack.last_mut().unwrap().2;
                    let name = self
                        .main_stores()
                        .label_store
                        .get_or_insert(acc.name);
                    assert!(!w.children_names.contains(&name),"{:?}",name);
                    w.push_submodule(name, full_node);
                    // println!("dir: {}", &acc.name);
                }
            } else {
                panic!("never empty")
            }
        }
        root_full_node
    }

    /// oid : Oid of a dir surch that */src/main/java/ or */src/test/java/
    fn handle_java_src(
        &mut self,
        repository: &Repository,
        name: &[u8],
        oid: git2::Oid,
    ) -> java_tree_gen::Local {
        // use java_tree_gen::{hash32, EntryR, NodeIdentifier, NodeStore,};

        let dir_hash = hash32(&Type::Directory);

        let root_full_node;

        let tree = repository.find_tree(oid).unwrap();
        let prepared: Vec<BasicGitObjects> = tree.iter().rev().map(Into::into).collect();
        let mut stack: Vec<(Oid, Vec<BasicGitObjects>, JavaAcc)> = vec![(
            oid,
            prepared,
            JavaAcc::new(std::str::from_utf8(&name).unwrap().to_string()),
        )];
        loop {
            if let Some(current_dir) = stack.last_mut().expect("never empty").1.pop() {
                match current_dir {
                    BasicGitObjects::Tree(x, name) => {
                        if let Some((already, skiped_ana)) = self.object_map_java.get(&x) {
                            // reinit already computed node for post order
                            let full_node = already.clone();

                            let name = self
                            .main_stores
                            .label_store
                            .get(std::str::from_utf8(&name).unwrap()).unwrap();
                            let n = self.main_stores.node_store.resolve(full_node.compressed_node);
                            let already_name = *n.get_label();
                            if name != already_name {
                                let already_name = self
                                    .main_stores()
                                    .label_store
                                    .resolve(&already_name)
                                    .to_string();
                                let name = self.main_stores().label_store.resolve(&name);
                                panic!("{} != {}", name, already_name);
                            } else if stack.is_empty() {
                                root_full_node = full_node;
                                break;
                            } else {
                                let w = &mut stack.last_mut().unwrap().2;
                                assert!(!w.children_names.contains(&name));
                                w.push_dir(name, full_node,*skiped_ana);
                            }
                            continue;
                        }
                        // TODO use maven pom.xml to find source_dir  and tests_dir ie. ignore resources, maybe also tests
                        println!("tree {:?}", std::str::from_utf8(&name));
                        let a = repository.find_tree(x).unwrap();
                        let prepared: Vec<BasicGitObjects> =
                            a.iter().rev().map(Into::into).collect();
                        stack.push((
                            x,
                            prepared,
                            JavaAcc::new(std::str::from_utf8(&name).unwrap().to_string()),
                        ));
                    }
                    BasicGitObjects::Blob(x, name) => {
                        if !Self::is_handled(&name) {
                            continue;
                        } else if let Some((already, _)) = self.object_map_java.get(&x) {
                            // TODO reinit already computed node for post order
                            let full_node = already.clone();

                            let name = self
                            .main_stores()
                            .label_store
                            .get_or_insert(std::str::from_utf8(&name).unwrap());
                            let n = self.main_stores().node_store.resolve(full_node.compressed_node);
                            let already_name = *n.get_label();
                            if name != already_name {
                                let already_name = self
                                    .main_stores()
                                    .label_store
                                    .resolve(&already_name)
                                    .to_string();
                                let name = self.main_stores().label_store.resolve(&name);
                                panic!("{} != {}", name, already_name);
                            } else if stack.is_empty() {
                                root_full_node = full_node;
                                break;
                            } else {
                                let w = &mut stack.last_mut().unwrap().2;
                                assert!(!w.children_names.contains(&name));
                                w.push(name,full_node);
                            }
                            continue;
                        }
                        println!("blob {:?}", std::str::from_utf8(&name));
                        // if std::str::from_utf8(&name).unwrap().eq("package-info.java") {
                        //     println!("module info:  {:?}", std::str::from_utf8(&name));
                        // } else
                        if std::str::from_utf8(&name).unwrap().ends_with(".java") {
                            let a = repository.find_blob(x).unwrap();
                            if let Ok(z) = std::str::from_utf8(a.content()) {
                                // log::debug!("content: {}", z);
                                let text = a.content();
                                if let Ok(full_node) =
                                    handle_java_file(&mut self.java_generator(text), &name, text)
                                {
                                    let full_node = full_node.local;
                                    // log::debug!("gen java");
                                    self.object_map_java
                                        .insert(a.id(), (full_node.clone(), false));
                                    let w = &mut stack.last_mut().unwrap().2;
                                    let name = self
                                    .main_stores()
                                    .label_store
                                    .get_or_insert(std::str::from_utf8(&name).unwrap());
                                    assert!(!w.children_names.contains(&name));
                                    w.push(name,full_node);
                                }
                            }
                        } else {
                            log::debug!("not java source file {:?}", std::str::from_utf8(&name));
                        }
                    }
                }
            } else if let Some((id, _, acc)) = stack.pop() {
                // commit node

                let hashed_label = hash32(&acc.name);

                let hsyntax = hashed::inner_node_hash(
                    &dir_hash,
                    &0,
                    &acc.metrics.size,
                    &acc.metrics.hashs.syntax,
                );
                let label = self
                    .main_stores()
                    .label_store
                    .get_or_insert(acc.name.clone());

                let eq = |x: EntryRef| {
                    let t = x.get_component::<Type>().ok();
                    if &t != &Some(&Type::Directory) {
                        return false;
                    }
                    let l = x.get_component::<java_tree_gen::LabelIdentifier>().ok();
                    if l != Some(&label) {
                        return false;
                    } else {
                        let cs = x.get_component::<Vec<NodeIdentifier>>().ok();
                        let r = cs == Some(&acc.children);
                        if !r {
                            return false;
                        }
                    }
                    true
                };
                let hashs = SyntaxNodeHashs {
                    structt: hashed::inner_node_hash(
                        &dir_hash,
                        &0,
                        &acc.metrics.size,
                        &acc.metrics.hashs.structt,
                    ),
                    label: hashed::inner_node_hash(
                        &dir_hash,
                        &hashed_label,
                        &acc.metrics.size,
                        &acc.metrics.hashs.label,
                    ),
                    syntax: hsyntax,
                };
                let ana = {
                    let ana = acc.ana;
                    let c = ana.refs_count();
                    log::info!("ref count in dir {}", c);
                    log::debug!("refs in directory");
                    for x in ana.display_refs(&self.main_stores().label_store) {
                        println!("    {}", x);
                    }
                    log::debug!("decls in directory");
                    for x in ana.display_decls(&self.main_stores().label_store) {
                        println!("    {}", x);
                    }
                    if c < MAX_REFS {
                        ana.resolve()
                    } else {
                        ana
                    }
                };
                log::info!("ref count in dir after resolver {}", ana.refs_count());
                log::debug!("refs in directory after resolve: ");
                for x in ana.display_refs(&self.main_stores().label_store) {
                    println!("    {}", x);
                }
                let insertion = self
                    .main_stores()
                    .node_store
                    .prepare_insertion(&hsyntax, eq);
                let node_id = if let Some(id) = insertion.occupied_id() {
                    id
                } else {
                    let vacant = insertion.vacant();
                    macro_rules! insert {
                        ( $c:expr, $t:ty ) => {
                            NodeStore::insert_after_prepare(
                                vacant,
                                $c.concat((<$t>::SIZE, <$t>::from(ana.refs()))),
                            )
                        };
                    }
                    // NodeStore::insert_after_prepare(
                    //     vacant,
                    //     (
                    //         Type::Directory,
                    //         label,
                    //         hashs,
                    //         CS(acc.children),
                    //         BloomSize::Much,
                    //     ),
                    // )
                    match acc.children.len() {
                        0 => NodeStore::insert_after_prepare(
                            vacant,
                            (Type::Directory, label, hashs, BloomSize::None),
                        ),
                        _ => {
                            assert_eq!(acc.children_names.len(),acc.children.len());
                            let c = (
                                Type::Directory,
                                label,
                                compo::Size(acc.metrics.size + 1),
                                compo::Height(acc.metrics.height + 1),
                                hashs,
                                CS(acc.children_names),
                                CS(acc.children),
                            );
                            match ana.refs_count() {
                                x if x > 2048 || acc.skiped_ana => NodeStore::insert_after_prepare(
                                    vacant,
                                    c.concat((BloomSize::Much,)),
                                ),
                                x if x > 1024 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 32]>)
                                }
                                x if x > 512 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 32]>)
                                }
                                x if x > 256 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 16]>)
                                    //1024
                                }
                                x if x > 150 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 8]>)
                                }
                                x if x > 100 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 4]>)
                                }
                                x if x > 30 => {
                                    insert!(c, Bloom::<&'static [u8], [u64; 2]>)
                                }
                                x if x > 15 => {
                                    insert!(c, Bloom::<&'static [u8], u64>)
                                }
                                x if x > 8 => {
                                    insert!(c, Bloom::<&'static [u8], u32>)
                                }
                                x if x > 0 => {
                                    insert!(c, Bloom::<&'static [u8], u16>)
                                }
                                _ => NodeStore::insert_after_prepare(
                                    vacant,
                                    c.concat((BloomSize::None,)),
                                ),
                            }
                        }
                    }
                };

                let metrics = java_tree_gen_full_compress_legion_ref::SubTreeMetrics {
                    size: acc.metrics.size + 1,
                    height: acc.metrics.height + 1,
                    hashs,
                };

                let full_node = java_tree_gen::Local {
                    compressed_node: node_id.clone(),
                    metrics,
                    ana: Some(ana.clone()),
                };
                self.object_map_java
                    .insert(id, (full_node.clone(), acc.skiped_ana));
                if stack.is_empty() {
                    root_full_node = full_node;
                    break;
                } else {
                    let w = &mut stack.last_mut().unwrap().2;
                    let name = self
                    .main_stores()
                    .label_store
                    .get_or_insert(acc.name.clone());
                    assert!(!w.children_names.contains(&name));
                    w.push_dir(name, full_node.clone(), acc.skiped_ana);
                    println!("dir: {}", &acc.name);
                }
            } else {
                panic!("never empty")
            }
        }
        root_full_node
    }
    pub fn child_by_name(&self, d: NodeIdentifier, name: &str) -> Option<NodeIdentifier> {
        let n = self.main_stores.node_store.resolve(d);
        n.get_child_by_name(&self.main_stores.label_store.get(name)?)
        // let s = n
        //     .get_children()
        //     .iter()
        //     .find(|x| {
        //         let n = self.main_stores.node_store.resolve(**x);

        //         if n.has_label() {
        //             self.main_stores.label_store.resolve(n.get_label()).eq(name)
        //         } else {
        //             false
        //         }
        //     })
        //     .map(|x| *x);
        // s
    }
    pub fn child_by_name_with_idx(
        &self,
        d: NodeIdentifier,
        name: &str,
    ) -> Option<(NodeIdentifier, usize)> {
        let n = self.main_stores.node_store.resolve(d);
        println!("{}",name);
        let i = n.get_child_idx_by_name(&self.main_stores.label_store.get(name)?);
        i.map(|i|(n.get_child(&i),i as usize))
        // let s = n
        //     .get_children()
        //     .iter()
        //     .enumerate()
        //     .find(|(_, x)| {
        //         let n = self.main_stores.node_store.resolve(**x);
        //         if n.has_label() {
        //             self.main_stores.label_store.resolve(n.get_label()).eq(name)
        //         } else {
        //             false
        //         }
        //     })
        //     .map(|(i, x)| (*x, i));
        // s
    }
    pub fn child_by_type(&self, d: NodeIdentifier, t: &Type) -> Option<(NodeIdentifier, usize)> {
        let n = self.main_stores.node_store.resolve(d);
        let s = n
            .get_children()
            .iter()
            .enumerate()
            .find(|(_, x)| {
                let n = self.main_stores.node_store.resolve(**x);
                n.get_type().eq(t)
            })
            .map(|(i, x)| (*x, i));
        s
    }

    pub fn print_matched_references(
        &self,
        ana: &mut PartialAnalysis,
        i: RefPtr,
        root: NodeIdentifier,
    ) {
        todo!()
        // for d in IterMavenModules::new(&self.main_stores, root) {
        //     let s = self.child_by_name(d, "src");
        //     let s = s.and_then(|d| self.child_by_name(d, "main"));
        //     let s = s.and_then(|d| self.child_by_name(d, "java"));
        //     // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
        //     if let Some(s) = s {
        //         // let n = self.main_stores.node_store.resolve(d);
        //         // println!(
        //         //     "search in module/src/main/java {}",
        //         //     self
        //         //         .main_stores
        //         //         .label_store
        //         //         .resolve(n.get_label())
        //         // );
        //         usage::find_refs(
        //             &self.main_stores,
        //             ana,
        //             &mut StructuralPosition::new(s),
        //             i,
        //             s,
        //         );
        //     }
        //     let s = self.child_by_name(d, "src");
        //     let s = s.and_then(|d| self.child_by_name(d, "test"));
        //     let s = s.and_then(|d| self.child_by_name(d, "java"));
        //     // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
        //     if let Some(s) = s {
        //         // let n = self.main_stores.node_store.resolve(d);
        //         // println!(
        //         //     "search in module/src/test/java {}",
        //         //     self
        //         //         .main_stores
        //         //         .label_store
        //         //         .resolve(n.get_label())
        //         // );
        //         usage::find_refs(
        //             &self.main_stores,
        //             ana,
        //             &mut StructuralPosition::new(s),
        //             i,
        //             s,
        //         );
        //     }
        // }
    }

    pub fn print_references_to_declarations_aux(
        &self,
        ana: &mut PartialAnalysis,
        s: NodeIdentifier,
    ) {
        todo!()
        // let mut d_it = IterDeclarations::new(&self.main_stores, s);
        // loop {
        //     if let Some(x) = d_it.next() {
        //         let b = self.main_stores.node_store.resolve(x);
        //         let t = b.get_type();
        //         let now = Instant::now();
        //         if &t == &Type::ClassDeclaration {
        //             let mut position =
        //                 extract_position(&self.main_stores, d_it.parents(), d_it.offsets());
        //             position.set_len(b.get_bytes_len(0) as usize);
        //             println!("now search for {:?} at {:?}", &t, position);
        //             {
        //                 let i = ana.solver.intern(RefsEnum::MaybeMissing);
        //                 let i = ana.solver.intern(RefsEnum::This(i));
        //                 println!("try search this");
        //                 usage::find_refs(&self.main_stores, ana, &mut d_it.position(x), i, x);
        //             }
        //             let mut l = None;
        //             for xx in b.get_children() {
        //                 let bb = self.main_stores.node_store.resolve(*xx);
        //                 if bb.get_type() == Type::Identifier {
        //                     let i = bb.get_label();
        //                     l = Some(*i);
        //                 }
        //             }
        //             if let Some(i) = l {
        //                 let o = ana.solver.intern(RefsEnum::MaybeMissing);
        //                 let f = self.main_stores.label_store.resolve(&i);
        //                 println!("search uses of {:?}", f);
        //                 let f = IdentifierFormat::from(f);
        //                 let l = LabelPtr::new(i, f);
        //                 let i = ana.solver.intern(RefsEnum::ScopedIdentifier(o, l));
        //                 println!("try search {:?}", ana.solver.nodes.with(i));
        //                 usage::find_refs(&self.main_stores, ana, &mut d_it.position(x), i, x);
        //                 {
        //                     let i = ana.solver.intern(RefsEnum::This(i));
        //                     println!("try search {:?}", ana.solver.nodes.with(i));
        //                     usage::find_refs(&self.main_stores, ana, &mut d_it.position(x), i, x);
        //                 }
        //                 let mut parents = d_it.parents().to_vec();
        //                 let mut offsets = d_it.offsets().to_vec();
        //                 let mut curr = parents.pop();
        //                 offsets.pop();
        //                 let mut prev = curr;
        //                 let mut before_p_ref = i;
        //                 let mut max_qual_ref = i;
        //                 let mut conti = false;
        //                 // go through classes if inner
        //                 loop {
        //                     if let Some(xx) = curr {
        //                         let bb = self.main_stores.node_store.resolve(xx);
        //                         let t = bb.get_type();
        //                         if t.is_type_body() {
        //                             println!(
        //                                 "try search {:?}",
        //                                 ana.solver.nodes.with(max_qual_ref)
        //                             );
        //                             usage::find_refs(
        //                                 &self.main_stores,
        //                                 ana,
        //                                 &mut (parents.clone(), offsets.clone(), x).into(),
        //                                 max_qual_ref,
        //                                 x,
        //                             );
        //                             prev = curr;
        //                             curr = parents.pop();
        //                             offsets.pop();
        //                             if let Some(xxx) = curr {
        //                                 let bb = self.main_stores.node_store.resolve(xxx);
        //                                 let t = bb.get_type();
        //                                 if t == Type::ObjectCreationExpression {
        //                                     conti = true;
        //                                     break;
        //                                 } else if !t.is_type_declaration() {
        //                                     panic!("{:?}", t);
        //                                 }
        //                                 let mut l2 = None;
        //                                 for xx in b.get_children() {
        //                                     let bb = self.main_stores.node_store.resolve(*xx);
        //                                     if bb.get_type() == Type::Identifier {
        //                                         let i = bb.get_label();
        //                                         l2 = Some(*i);
        //                                     }
        //                                 }
        //                                 if let Some(i) = l2 {
        //                                     let o = ana.solver.intern(RefsEnum::MaybeMissing);
        //                                     let f = IdentifierFormat::from(
        //                                         self.main_stores.label_store.resolve(&i),
        //                                     );
        //                                     let l = LabelPtr::new(i, f);
        //                                     let i =
        //                                         ana.solver.intern(RefsEnum::ScopedIdentifier(o, l));
        //                                     max_qual_ref = ana
        //                                         .solver
        //                                         .try_solve_node_with(max_qual_ref, i)
        //                                         .unwrap();
        //                                     println!(
        //                                         "try search {:?}",
        //                                         ana.solver.nodes.with(max_qual_ref)
        //                                     );
        //                                     usage::find_refs(
        //                                         &self.main_stores,
        //                                         ana,
        //                                         &mut (parents.clone(), offsets.clone(), xx).into(),
        //                                         max_qual_ref,
        //                                         xx,
        //                                     );
        //                                 }
        //                                 prev = curr;
        //                                 curr = parents.pop();
        //                                 offsets.pop();
        //                             }
        //                         } else if t == Type::Program {
        //                             // go through program i.e. package declaration
        //                             before_p_ref = max_qual_ref;
        //                             for xx in b.get_children() {
        //                                 let bb = self.main_stores.node_store.resolve(*xx);
        //                                 let t = bb.get_type();
        //                                 if t == Type::PackageDeclaration {
        //                                     let p = remake_pkg_ref(&self.main_stores, ana, *xx);
        //                                     max_qual_ref = ana
        //                                         .solver
        //                                         .try_solve_node_with(max_qual_ref, p)
        //                                         .unwrap();
        //                                 } else if t.is_type_declaration() {
        //                                     println!(
        //                                         "try search {:?}",
        //                                         ana.solver.nodes.with(max_qual_ref)
        //                                     );
        //                                     if Some(*xx) != prev {
        //                                         usage::find_refs(
        //                                             &self.main_stores,
        //                                             ana,
        //                                             &mut (parents.clone(), offsets.clone(), *xx)
        //                                                 .into(),
        //                                             before_p_ref,
        //                                             *xx,
        //                                         );
        //                                     }
        //                                     usage::find_refs(
        //                                         &self.main_stores,
        //                                         ana,
        //                                         &mut (parents.clone(), offsets.clone(), *xx).into(),
        //                                         max_qual_ref,
        //                                         *xx,
        //                                     );
        //                                 }
        //                             }
        //                             prev = curr;
        //                             curr = parents.pop();
        //                             offsets.pop();
        //                             break;
        //                         } else if t == Type::ObjectCreationExpression {
        //                             conti = true;
        //                             break;
        //                         } else if t == Type::Block {
        //                             conti = true;
        //                             break; // TODO check if really done
        //                         } else {
        //                             todo!("{:?}", t)
        //                         }
        //                     }
        //                 }
        //                 if conti {
        //                     continue;
        //                 }
        //                 // go through package
        //                 if let Some(xx) = curr {
        //                     if Some(xx) != prev {
        //                         usage::find_refs(
        //                             &self.main_stores,
        //                             ana,
        //                             &mut (parents.clone(), offsets.clone(), xx).into(),
        //                             before_p_ref,
        //                             xx,
        //                         );
        //                     }
        //                     usage::find_refs(
        //                         &self.main_stores,
        //                         ana,
        //                         &mut (parents.clone(), offsets.clone(), xx).into(),
        //                         max_qual_ref,
        //                         xx,
        //                     );
        //                     prev = curr;
        //                     curr = parents.pop();
        //                     offsets.pop();
        //                 }
        //                 // go through directories
        //                 loop {
        //                     if let Some(xx) = curr {
        //                         if Some(xx) != prev {
        //                             usage::find_refs(
        //                                 &self.main_stores,
        //                                 ana,
        //                                 &mut (parents.clone(), offsets.clone(), xx).into(),
        //                                 max_qual_ref,
        //                                 xx,
        //                             );
        //                         }
        //                         prev = curr;
        //                         curr = parents.pop();
        //                         offsets.pop();
        //                     } else {
        //                         break;
        //                     }
        //                 }
        //             }

        //             println!("time taken for refs search: {}", now.elapsed().as_nanos());
        //         } else {
        //             // TODO
        //             // println!("todo impl search on {:?}", &t);
        //         }

        //         // println!("it state {:?}", &d_it);
        //         // java_tree_gen_full_compress_legion_ref::print_tree_syntax(
        //         //     &self.main_stores.node_store,
        //         //     &self.main_stores.label_store,
        //         //     &x,
        //         // );
        //         // println!();
        //     } else {
        //         break;
        //     }
        // }
    }
    pub fn print_references_to_declarations(
        &self,
        ana: &mut PartialAnalysis,
        root: NodeIdentifier,
    ) {
        let mut m_it = IterMavenModules::new(&self.main_stores, root);
        loop {
            let d = if let Some(d) = m_it.next() { d } else { break };
            // m_it.parents();
            let src = self.child_by_name(d, "src");

            let s = src.and_then(|d| self.child_by_name(d, "main"));
            let s = s.and_then(|d| self.child_by_name(d, "java"));
            // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
            if let Some(s) = s {
                // let n = self.main_stores.node_store.resolve(d);
                // println!(
                //     "search in module/src/main/java {}",
                //     self
                //         .main_stores
                //         .label_store
                //         .resolve(n.get_label())
                // );
                // usage::find_all_decls(&self.main_stores, ana, s);
                self.print_references_to_declarations_aux(ana, s)
            }
            let s = src.and_then(|d| self.child_by_name(d, "test"));
            let s = s.and_then(|d| self.child_by_name(d, "java"));
            // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
            if let Some(s) = s {
                // let n = self.main_stores.node_store.resolve(d);
                // println!(
                //     "search in module/src/test/java {}",
                //     self
                //         .main_stores
                //         .label_store
                //         .resolve(n.get_label())
                // );
                // let mut d_it = IterDeclarations::new(&self.main_stores, s);
                self.print_references_to_declarations_aux(ana, s)
            }
        }
    }

    pub fn print_declarations(&self, ana: &mut PartialAnalysis, root: NodeIdentifier) {
        for d in IterMavenModules::new(&self.main_stores, root) {
            let s = self.child_by_name(d, "src");
            let s = s.and_then(|d| self.child_by_name(d, "main"));
            let s = s.and_then(|d| self.child_by_name(d, "java"));
            // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
            if let Some(s) = s {
                // let n = self.main_stores.node_store.resolve(d);
                // println!(
                //     "search in module/src/main/java {}",
                //     self
                //         .main_stores
                //         .label_store
                //         .resolve(n.get_label())
                // );
                // usage::find_all_decls(&self.main_stores, ana, s);
                let mut d_it = IterDeclarations::new(&self.main_stores, s);
                loop {
                    if let Some(x) = d_it.next() {
                        let b = self.main_stores.node_store.resolve(x);
                        let t = b.get_type();
                        println!("now search for {:?}", &t);
                        println!("it state {:?}", &d_it);
                        // java_tree_gen_full_compress_legion_ref::print_tree_syntax(
                        //     &self.main_stores.node_store,
                        //     &self.main_stores.label_store,
                        //     &x,
                        // );
                        // println!();
                    } else {
                        break;
                    }
                }
            }
            let s = self.child_by_name(d, "src");
            let s = s.and_then(|d| self.child_by_name(d, "test"));
            let s = s.and_then(|d| self.child_by_name(d, "java"));
            // let s = s.and_then(|d| self.child_by_type(d, &Type::Directory));
            if let Some(s) = s {
                // let n = self.main_stores.node_store.resolve(d);
                // println!(
                //     "search in module/src/test/java {}",
                //     self
                //         .main_stores
                //         .label_store
                //         .resolve(n.get_label())
                // );
                let mut d_it = IterDeclarations::new(&self.main_stores, s);
                loop {
                    if let Some(x) = d_it.next() {
                        let b = self.main_stores.node_store.resolve(x);
                        let t = b.get_type();
                        println!("now search for {:?}", &t);
                        println!("it state {:?}", &d_it);
                        // java_tree_gen_full_compress_legion_ref::print_tree_syntax(
                        //     &self.main_stores.node_store,
                        //     &self.main_stores.label_store,
                        //     &x,
                        // );
                        // println!();
                    } else {
                        break;
                    }
                }
            }
        }
    }
}

fn drain_filter_strip(v: &mut Option<Vec<PathBuf>>, name: &[u8]) -> Vec<PathBuf> {
    let mut new_sub_modules = vec![];
    if let Some(sub_modules) = v {
        sub_modules
            .drain_filter(|x| {
                // x.components().next().map_or(false, |s| {
                //     name.eq(std::os::unix::prelude::OsStrExt::as_bytes(
                //         s.as_os_str(),
                //     ))
                // })
                x.starts_with(std::str::from_utf8(&name).unwrap())
            })
            .for_each(|x| {
                let x = x
                    .strip_prefix(std::str::from_utf8(&name).unwrap())
                    .unwrap()
                    .to_owned();
                new_sub_modules.push(x);
            });
    }
    new_sub_modules
}
