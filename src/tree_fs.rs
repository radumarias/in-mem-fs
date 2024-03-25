use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use bytebuffer::ByteBuffer;
use crate::tree::{Tree, TreeNode};

pub struct Item<T> {
    pub ino: u64,
    pub name: String,
    pub is_dir: bool,
    pub extra: Option<T>,
    pub data: Option<ByteBuffer>,
    node: Option<Rc<RefCell<TreeNode<Item<T>>>>>,
}

impl<T> Item<T> {
    pub fn new(ino: u64, name: String, is_dir: bool, extra: Option<T>) -> Self {
        Item {
            ino,
            name,
            is_dir,
            extra,
            data: Some(ByteBuffer::new()),
            node: None,
        }
    }
    pub fn children(&self) -> Vec<&Item<T>> {
        if !self.is_dir {
            return vec![];
        }

        self.node.as_ref().unwrap().borrow_mut().iter()
            .map(|node| unsafe { &(*node.as_ptr()).value })
            .collect()
    }

    pub fn get_parent(&self) -> Option<&Item<T>> {
        self.node.as_ref().unwrap().borrow_mut().get_parent().map(|parent| unsafe { &(*parent.as_ptr()).value })
    }

    pub fn find_child_mut<'a, 'b>(&'b self, name: &str) -> Option<&'a mut Item<T>> {
        if !self.is_dir {
            return None;
        }

        self.node.as_ref().unwrap().borrow_mut().iter()
            .find(|node| node.borrow().value.name == name)
            .map(|node| unsafe { &mut (*node.as_ptr()).value })
    }
}

pub struct TreeFs<T> {
    tree: Tree<Item<T>>,
    ino_to_node: HashMap<u64, Rc<RefCell<TreeNode<Item<T>>>>>,
}

impl<T> TreeFs<T> {
    pub fn new() -> Self {
        TreeFs {
            tree: Tree::new(),
            ino_to_node: HashMap::new(),
        }
    }

    pub fn set_root<'b, 'c>(&'c mut self, item: Item<T>) -> &'b Item<T> {
        match item {
            Item { name: _, is_dir: true, .. } => {
                let root = TreeNode::new(item);
                self.tree.set_root(root.clone());

                // link Item to TreeNode
                root.borrow_mut().value.node = Some(root.clone());

                // add it to ino -> Item map
                self.ino_to_node.insert(root.borrow().value.ino, root.clone());

                unsafe {
                    &(*root.as_ptr()).value
                }
            }
            _ => { panic!("Root must be a directory") }
        }
    }

    pub fn get_root(&self) -> Option<&Item<T>> {
        self.tree.get_root().map(|root| unsafe { &(*root.as_ptr()).value })
    }

    pub fn push<'b, 'c>(&'c mut self, parent: &Item<T>, child: Item<T>) -> &'b Item<T> {
        match parent {
            Item { name: _, is_dir: true, .. } => {
                let parent_node = parent.node.as_ref().unwrap().clone();
                let child_node = TreeNode::new(child);
                self.tree.push_child(&parent_node, &child_node);

                // link Item to TreeNode
                parent_node.borrow_mut().iter_mut().rev().next().unwrap().borrow_mut().value.node = Some(child_node.clone());

                // add it to ino -> Item map
                self.ino_to_node.insert(child_node.borrow().value.ino, child_node.clone());

                unsafe {
                    &(*child_node.as_ptr()).value
                }
            }
            _ => { panic!("Parent must be a directory") }
        }
    }

    pub fn remove_child(&mut self, parent: &Item<T>, child: &Item<T>) {
        match parent {
            Item { name: _, is_dir: true, .. } => {
                let parent_node = child.node.as_ref().unwrap().borrow().get_parent().unwrap();
                // check if parent contains the child
                if !Rc::ptr_eq(&parent_node, &parent.node.as_ref().unwrap()) {
                    panic!("Parent does not contain the child");
                }
                self.tree.remove_child(&parent_node, &child.node.as_ref().unwrap());

                self.ino_to_node.remove(&child.ino);
            }
            _ => { panic!("Parent must be a directory") }
        }
    }

    pub fn get_item_mut<'a, 'b>(&'b mut self, ino: u64) -> Option<&'a mut Item<T>> {
        self.ino_to_node.get(&ino).map(|item| unsafe {&mut (*item.as_ptr()).value})
    }
}