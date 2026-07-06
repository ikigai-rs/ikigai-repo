//! ikigai reading its own git state through the manifold.
//!
//!   cargo run --example repo-demo
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::sync::Arc;

fn main() {
    let kernel = Kernel::new(Arc::new(ikigai_repo::space()));
    // A capability scoped to exactly "may run git" — nothing else.
    let cap = Capability::scoped(["urn:cap:exec:git"]);

    for iri in ["urn:repo:branch", "urn:repo:status", "urn:repo:log"] {
        let req = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        match futures::executor::block_on(kernel.issue(req, &cap)) {
            Ok(repr) => {
                let body = String::from_utf8_lossy(&repr.bytes);
                println!("\n$ {iri}\n{}", body.trim_end());
            }
            Err(e) => println!("\n$ {iri}\n  error: {e:?}"),
        }
    }
}
