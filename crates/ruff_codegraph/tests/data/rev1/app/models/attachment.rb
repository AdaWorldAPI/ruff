class Attachment < Comment
  attribute :size, :integer

  def label
    self.name
  end
end
