using System;
using System.Text;
using System.Windows.Forms;

// Synthetic WinForms navigation fixture — exercises the `navigates_to` arm
// (EmitNavArm). GENERIC screen names only: the harvester machinery is agnostic
// and real corpora are pointed at via a path, never vendored (README
// "Provenance / non-vendoring"). Expected edges:
//   MainScreen -> OrderScreen      (one-liner  new X().Show())
//   MainScreen -> SettingsScreen   (one-liner  new X().ShowDialog())
//   MainScreen -> CustomerScreen   (two-stmt   var f = new X(); ...; f.Show())
//   HostControl -> ChildControl    (UserControl-SPA: field = new X(); NO .Show())
//   QualifiedHost -> QualifiedChild (qualified: field = new Ns.X(); qualifier stripped)
// SaveFileDialog is a framework CommonDialog and must NOT produce an edge; a
// non-screen helper (`new StringBuilder()`) must NOT produce an edge either.
namespace NavShapes
{
    public class MainScreen : Form
    {
        // one-liner: new TargetForm().Show()
        private void OpenOrders_Click(object sender, EventArgs e)
        {
            new OrderScreen().Show();
        }

        // one-liner modal: new TargetForm().ShowDialog()
        private void OpenSettings_Click(object sender, EventArgs e)
        {
            new SettingsScreen().ShowDialog();
        }

        // two-statement: local tracked, then .Show()
        private void OpenCustomer_Click(object sender, EventArgs e)
        {
            var f = new CustomerScreen();
            f.Text = "Customer";
            f.Show();
        }

        // framework CommonDialog — must NOT emit a navigates_to edge
        private void Save_Click(object sender, EventArgs e)
        {
            var dlg = new SaveFileDialog();
            dlg.ShowDialog();
        }
    }

    public class OrderScreen : Form { }

    public class SettingsScreen : Form { }

    public class CustomerScreen : Form { }

    // UserControl-SPA host: navigates by instantiating a screen-typed
    // UserControl into a field — NO `.Show()`. This is the idiom a ribbon /
    // panel-swap app uses (and the one the `.Show()`-only harvester missed).
    public class HostControl : UserControl
    {
        private ChildControl _child;

        private void Open_Click(object sender, EventArgs e)
        {
            _child = new ChildControl(this);   // -> navigates_to ChildControl
            var sb = new StringBuilder();       // non-screen: NO edge
            sb.Append(_child.Name);
        }
    }

    public class ChildControl : UserControl
    {
        public ChildControl(Control owner) { }
    }

    // Namespace-qualified hosting — the dominant idiom in namespace-organized
    // apps: `field = new Nested.QualifiedChild(...)`. The nav arm normalizes
    // the qualifier away (LastSegment), so this resolves to the SAME screen
    // node as a bare `new QualifiedChild(...)` would:
    //   QualifiedHost -> QualifiedChild
    public class QualifiedHost : UserControl
    {
        private Nested.QualifiedChild _q;

        private void Open_Click(object sender, EventArgs e)
        {
            _q = new Nested.QualifiedChild(this);  // -> navigates_to QualifiedChild
        }
    }

    // Ribbon/tab selector idiom: navigate by assigning a nav-selector property
    // (the panel-swap top-nav — no `.Show()`, no `new Screen()`).
    //   RibbonHost -selects_view-> tab_reports  (ribbon.SelectedRibbonTabItem = ...)
    // A non-selector property (`SelectedIndex`) must NOT emit an edge.
    public class RibbonHost : Form
    {
        private void Nav_Click(object sender, EventArgs e)
        {
            ribbon.SelectedRibbonTabItem = tab_reports;  // -> selects_view tab_reports
            combo.SelectedIndex = fallbackIndex;          // SelectedIndex not a nav selector: NO edge
        }
    }
}

namespace NavShapes.Nested
{
    public class QualifiedChild : System.Windows.Forms.UserControl
    {
        public QualifiedChild(object owner) { }
    }
}
