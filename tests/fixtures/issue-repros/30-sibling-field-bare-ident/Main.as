// Minimal repro for GH #30 (false negative):
// A bare identifier inside a class method must NOT resolve to a *sibling*
// class's field. Fields are stored as `ClassName::field` in the workspace
// symbol table; unqualified bare-name tail matching used to leak
// `ItemModelTreeElement::nod` into `ItemModel`, silencing the
// `undefined identifier` the game compiler emits (`No matching symbol 'nod'`).

class ItemModelTreeElement {
    CMwNod@ nod;
    void Use() {
        if (nod is null) return; // legal: own field
    }
}

class ItemModel {
    CGameItemModel@ item;
    void NullifyEMEAndTransformSurfaces() {
        auto bad = cast<CGameItemModel>(nod); // GH #30: game rejects; LSP must flag
        if (bad is null) return;
    }
}

void Main() {}
