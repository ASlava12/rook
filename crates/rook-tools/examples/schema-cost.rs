fn main() {
    let mut tools = rook_tools::ToolBox::standard();
    tools
        .register(std::sync::Arc::new(rook_tools::ask::AskUser(std::sync::Arc::new(rook_tools::ask::NoOne))));
    let estimate = |t: &rook_llm::ToolSpec| {
        (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4)
    };
    let mut full: Vec<_> = tools.specs().iter().map(|t| (t.name.clone(), estimate(t))).collect();
    full.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let total: usize = full.iter().map(|(_, n)| n).sum();
    let stubs: usize = tools.stubs().iter().map(estimate).sum();
    for (name, cost) in &full {
        println!("  {name:<14} ~{cost} tok");
    }
    println!("  {:-<26}", "");
    println!("  full schemas   ~{total} tok/call");
    println!("  stubs only     ~{stubs} tok/call");
}
