use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::slice::{Iter, IterMut};
use std::vec::IntoIter;

pub struct TreeNode<T> {
    pub value: T,
    children: Vec<Rc<RefCell<TreeNode<T>>>>,
    parent: Weak<RefCell<TreeNode<T>>>,
}

impl<T> TreeNode<T> {
    pub fn new(value: T) -> Rc<RefCell<TreeNode<T>>> {
        Rc::new(RefCell::new(TreeNode {
            value,
            children: Vec::new(),
            parent: Weak::new(),
        }))
    }

    pub fn get_parent(&self) -> Option<Rc<RefCell<TreeNode<T>>>> {
        self.parent.upgrade()
    }

    pub fn iter(&mut self) -> Iter<'_, Rc<RefCell<TreeNode<T>>>> {
        self.children.iter()
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, Rc<RefCell<TreeNode<T>>>> {
        self.children.iter_mut()
    }

    pub fn into_iter(self) -> IntoIter<Rc<RefCell<TreeNode<T>>>> {
        self.children.into_iter()
    }
}

pub struct Tree<T> {
    root: Option<Rc<RefCell<TreeNode<T>>>>,
}

impl<T> Tree<T> {
    pub fn new() -> Self {
        Tree { root: None }
    }
    pub fn set_root(&mut self, root: Rc<RefCell<TreeNode<T>>>) {
        self.root = Some(root);
    }

    pub fn get_root(&self) -> Option<Rc<RefCell<TreeNode<T>>>> {
        self.root.clone()
    }

    pub fn push_child(&self, parent: &Rc<RefCell<TreeNode<T>>>, child: &Rc<RefCell<TreeNode<T>>>) {
        parent.borrow_mut().children.push(child.clone());
        child.borrow_mut().parent = Rc::downgrade(&parent);
    }

    pub fn remove_child(&self, parent: &Rc<RefCell<TreeNode<T>>>, child: &Rc<RefCell<TreeNode<T>>>) {
        parent.borrow_mut().children.retain(|c| !Rc::ptr_eq(c, &child));
        child.borrow_mut().parent = Weak::new();
    }
}